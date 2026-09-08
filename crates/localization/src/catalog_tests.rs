use std::collections::BTreeSet;

use super::{CatalogScope, CatalogSource, FormatError, Localizer, Message, format_template};
use crate::{
    ALL_MESSAGES, LANGUAGE_REGISTRY, LanguagePreference, LinuxLocaleEnvironment,
    MAX_FORMATTED_MESSAGE_BYTES, Preferences, TranslationReadiness, catalog_coverage,
    find_language, resolve_language,
};

fn localizer(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

#[test]
fn initial_key_ids_are_unique_and_both_catalogs_have_every_declared_message() {
    assert_eq!(ALL_MESSAGES.len(), 283);
    let ids: BTreeSet<_> = ALL_MESSAGES.iter().map(|message| message.id()).collect();
    assert_eq!(ids.len(), ALL_MESSAGES.len());
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        for &message in ALL_MESSAGES {
            let resolved = localizer.resolve(message);
            assert!(!resolved.text.is_empty(), "{tag}: {}", message.id());
            assert_eq!(resolved.source, CatalogSource::RequestedCatalog);
            assert_eq!(resolved.language_tag, tag);
        }
        let coverage = localizer.coverage();
        assert_eq!(coverage.translated, ALL_MESSAGES.len());
        assert_eq!(coverage.total, ALL_MESSAGES.len());
        assert_eq!(coverage.scope, CatalogScope::InitialUiSlice);
        assert!(!coverage.entire_ui_covered);
    }
}

#[test]
fn parameter_contracts_match_templates_in_both_languages() {
    for &message in ALL_MESSAGES {
        let names = message.parameters();
        let unique: BTreeSet<_> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len());
        for tag in ["en", "zh"] {
            let localizer = localizer(tag);
            let arguments: Vec<_> = names.iter().map(|&name| (name, name)).collect();
            let formatted = localizer.format(message, &arguments).unwrap();
            assert!(!formatted.is_empty());
            let template = localizer.text(message);
            for name in names {
                assert!(
                    template.contains(&format!("{{{name}}}")),
                    "{}",
                    message.id()
                );
                assert!(formatted.contains(name), "{}", message.id());
            }
            if names.is_empty() {
                assert_eq!(formatted, template);
            }
        }
    }
}

#[test]
fn remaining_27_languages_use_explicit_english_fallback_not_fake_coverage() {
    let english = localizer("en");
    let mut absent = 0;
    for language in LANGUAGE_REGISTRY {
        assert_eq!(language.readiness, TranslationReadiness::NotProvided);
        if matches!(language.tag, "en" | "zh") {
            continue;
        }
        absent += 1;
        let localizer = Localizer::new(language);
        assert_eq!(localizer.requested_language(), language);
        assert_eq!(catalog_coverage(language).translated, 0);
        assert!(!localizer.coverage().entire_ui_covered);
        for &message in ALL_MESSAGES {
            let resolved = localizer.resolve(message);
            assert_eq!(resolved.source, CatalogSource::EnglishFallback);
            assert_eq!(resolved.language_tag, "en");
            assert_eq!(resolved.text, english.text(message));
        }
    }
    assert_eq!(absent, 27);
}

#[test]
fn missing_individual_translation_is_fallback_and_not_a_translated_slot() {
    // Production catalogs are complete for this slice. Exercise the future
    // missing-entry path without shipping an empty placeholder as translated.
    let entry = super::Entry {
        id: "fixture-only",
        parameters: &[],
        english: "English baseline",
        chinese: "",
    };
    assert_eq!(entry.translation("zh"), None);
    let resolved = entry.resolve("zh");
    assert_eq!(resolved.text, "English baseline");
    assert_eq!(resolved.language_tag, "en");
    assert_eq!(resolved.source, CatalogSource::EnglishFallback);
}

#[test]
fn missing_duplicate_and_unknown_arguments_are_not_silently_ignored() {
    let localizer = localizer("zh");
    assert_eq!(
        localizer.format(Message::LanguageEffective, &[]),
        Err(FormatError::MissingArgument("language"))
    );
    assert_eq!(
        localizer.format(
            Message::LanguageEffective,
            &[("language", "en"), ("language", "zh")]
        ),
        Err(FormatError::DuplicateArgument("language"))
    );
    assert_eq!(
        localizer.format(Message::LanguageEffective, &[("Language", "zh")]),
        Err(FormatError::UnknownArgument)
    );
    assert_eq!(
        localizer.format(Message::CancelButton, &[("unexpected", "ignored?")]),
        Err(FormatError::UnknownArgument)
    );
}

#[test]
fn arguments_are_opaque_and_never_recursively_substituted_or_normalized() {
    let path = "../用户/{frames}/{{unknown}}/a \\\"字幕\\\".gif\n\u{0301}";
    let before = path.to_owned();
    let count = "{path}";
    for (tag, expected) in [
        (
            "en",
            format!("Export complete. Frames: {count}. File: {path}"),
        ),
        ("zh", format!("导出完成。帧数：{count}。文件：{path}")),
    ] {
        let result = localizer(tag)
            .format(
                Message::ExportFinished,
                &[("path", path), ("frames", count)],
            )
            .unwrap();
        assert_eq!(result, expected);
        assert_eq!(path, before);
    }
    assert_eq!(
        localizer("zh")
            .format(
                Message::HomeContinueEditing,
                &[("name", "字幕{unknown}.gfsproj")]
            )
            .unwrap(),
        "继续编辑 字幕{unknown}.gfsproj"
    );
}

#[test]
fn formatting_never_changes_requested_preference_or_raw_system_snapshot() {
    let preference = Preferences {
        language: LanguagePreference::explicit("fr-CA").unwrap(),
    };
    let before = serde_json::to_string(&preference).unwrap();
    let environment = LinuxLocaleEnvironment {
        lc_all: Some("zh_TW.UTF-8@modifier"),
        language: Some("fr_CA:zh_CN"),
        ..Default::default()
    };
    let original_environment = environment;
    let resolution = resolve_language(&preference.language, &environment);
    let localizer = Localizer::new(resolution.language);
    assert_eq!(
        localizer.resolve(Message::LanguageEffective).source,
        CatalogSource::EnglishFallback
    );
    assert_eq!(
        localizer
            .format(Message::LanguageEffective, &[("language", "fr-CA")])
            .unwrap(),
        "Current language: fr-CA"
    );
    assert_eq!(serde_json::to_string(&preference).unwrap(), before);
    assert_eq!(environment, original_environment);
}

#[test]
fn message_output_limit_is_byte_exact_without_truncating_user_values() {
    let localizer = localizer("en");
    let prefix = "Current language: ";
    let exact = "x".repeat(MAX_FORMATTED_MESSAGE_BYTES - prefix.len());
    let result = localizer
        .format(Message::LanguageEffective, &[("language", &exact)])
        .unwrap();
    assert_eq!(result.len(), MAX_FORMATTED_MESSAGE_BYTES);
    assert!(result.ends_with(&exact));
    let too_long = format!("{exact}中");
    assert_eq!(
        localizer.format(Message::LanguageEffective, &[("language", &too_long)]),
        Err(FormatError::OutputTooLarge)
    );
}

#[test]
fn template_brace_escaping_is_single_pass_and_invalid_templates_fail() {
    assert_eq!(
        format_template("{{literal}} {name}", &[("name", "{unknown}")]).unwrap(),
        "{literal} {unknown}"
    );
    for invalid in ["{", "}", "{unknown}", "{}", "{name", "{name}}"] {
        assert_eq!(
            format_template(invalid, &[("name", "value")]),
            Err(FormatError::InvalidTemplate)
        );
    }
}

#[test]
fn recorder_compact_buttons_do_not_change_existing_message_contracts() {
    let english = localizer("en");
    let chinese = localizer("zh");
    for (message, id, text, translated) in [
        (
            Message::RecorderStop,
            "recorder-stop",
            "Stop and save",
            "停止并保存",
        ),
        (
            Message::RecorderStopShort,
            "recorder-stop-short",
            "Stop",
            "停止",
        ),
        (
            Message::RecorderDiscard,
            "recorder-discard",
            "Discard recording",
            "丢弃录制",
        ),
        (
            Message::RecorderDiscardShort,
            "recorder-discard-short",
            "Discard",
            "丢弃",
        ),
        (Message::RecorderStart, "recorder-start", "Start", "开始"),
        (Message::RecorderPause, "recorder-pause", "Pause", "暂停"),
        (Message::RecorderResume, "recorder-resume", "Resume", "继续"),
        (Message::CancelButton, "cancel-button", "Cancel", "取消"),
    ] {
        assert_eq!(message.id(), id);
        assert!(message.parameters().is_empty());
        assert_eq!(english.text(message), text);
        assert_eq!(chinese.text(message), translated);
    }
    assert_eq!(Message::RecorderCountdown.parameters(), ["seconds"]);
    assert_eq!(
        english.text(Message::RecorderCountdown),
        "Starting in {seconds}…"
    );
    assert_eq!(
        english.text(Message::RecorderStartsIn),
        "Recording starts in {seconds}s"
    );
    assert_eq!(Message::RecorderStartsIn.parameters(), ["seconds"]);
}

#[test]
fn recorder_named_values_keep_signed_coordinates_precision_and_private_data_literal() {
    let arguments = [
        ("width", "1"),
        ("height", "720"),
        ("x", "-1920"),
        ("y", "-40"),
        ("kind", "Window {width} 用户标题"),
    ];
    assert_eq!(
        localizer("en")
            .format(Message::RecorderSelectedSource, &arguments)
            .unwrap(),
        "Selected source: 1×720 at -1920,-40 (Window {width} 用户标题)"
    );
    assert_eq!(
        localizer("zh")
            .format(Message::RecorderSelectedSource, &arguments)
            .unwrap(),
        "所选来源：1×720，位置 -1920,-40（Window {width} 用户标题）"
    );
    let path = "/home/用户/{frames}/还原{seconds}.gfsproj";
    for tag in ["en", "zh"] {
        let result = localizer(tag)
            .format(
                Message::RecorderProjectReady,
                &[("frames", "0"), ("seconds", "0.125"), ("path", path)],
            )
            .unwrap();
        assert!(result.contains(path));
        assert!(result.contains("0.125s"));
        let error = "BadWindow: /private/{error}/密码.gfsproj\nraw OS detail";
        let result = localizer(tag)
            .format(Message::SnapDragError, &[("error", error)])
            .unwrap();
        assert!(result.contains(error));
    }
}

#[test]
fn recorder_dynamic_contracts_reject_old_positional_or_incorrect_names() {
    let cases = [
        (Message::RecorderStartsIn, vec![("remaining", "3")]),
        (
            Message::RecorderProgressSummary,
            vec![("count", "2"), ("seconds", "0.5")],
        ),
        (Message::RecorderFixedDelayHint, vec![("ms", "66")]),
        (
            Message::RecorderCountdownRange,
            vec![("MAX_COUNTDOWN_SECONDS", "60")],
        ),
        (Message::SnapFound, vec![("windows", "2")]),
        (Message::SnapFailed, vec![("reason", "raw OS detail")]),
    ];
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        for (message, arguments) in &cases {
            assert_eq!(
                localizer.format(*message, arguments),
                Err(FormatError::UnknownArgument)
            );
        }
        assert_eq!(
            localizer.format(
                Message::RecorderProgressTimingHint,
                &[("source_seconds", "1.234")]
            ),
            Err(FormatError::MissingArgument("playback_seconds"))
        );
    }
}

#[test]
fn typed_phase_and_source_labels_preserve_original_english_without_debug_lookup() {
    for (message, english) in [
        (Message::RecorderPhaseStartingCapture, "StartingCapture"),
        (Message::RecorderPhaseCapturing, "Capturing"),
        (Message::RecorderPaused, "Paused"),
        (Message::RecorderPhaseStoppingCapture, "StoppingCapture"),
        (Message::RecorderPhaseEncoding, "Encoding"),
        (Message::RecorderPhaseCommitting, "Committing"),
        (Message::RecorderPhaseComplete, "Complete"),
        (Message::RecorderSourceMonitor, "Monitor"),
        (Message::RecorderSourceWindow, "Window"),
        (Message::RecorderUnknown, "Unknown"),
    ] {
        assert_eq!(localizer("en").text(message), english);
        assert_ne!(localizer("zh").text(message), english);
        assert!(message.parameters().is_empty());
    }
    assert_eq!(
        localizer("en")
            .format(Message::RecorderCountdownNotice, &[("seconds", "3")])
            .unwrap(),
        "Recording starts in 3 seconds…"
    );
    assert_eq!(
        localizer("zh")
            .format(Message::RecorderCountdownRange, &[("maximum", "60")])
            .unwrap(),
        "倒计时必须在 0 到 60 秒之间。"
    );
    assert_eq!(
        localizer("en")
            .format(
                Message::RecorderPreparedNotice,
                &[("width", "1"), ("height", "1")]
            )
            .unwrap(),
        "Wayland source prepared at 1×1 pixels. The native session is paused and retained by its worker."
    );
}

#[test]
fn source_discovery_keeps_protocol_names_and_raw_diagnostics_literal() {
    for server in ["X11", "Wayland"] {
        let arguments = [("count", "0"), ("display_server", server)];
        assert_eq!(
            localizer("en")
                .format(Message::RecorderSourcesFound, &arguments)
                .unwrap(),
            format!("Found 0 {server} capture source option(s).")
        );
        assert_eq!(
            localizer("zh")
                .format(Message::RecorderSourcesFound, &arguments)
                .unwrap(),
            format!("找到 0 个 {server} 录制来源选项。")
        );
    }
    let error = "PermissionDenied {display_server}: /run/user/用户/{count}";
    assert_eq!(
        localizer("zh")
            .format(Message::RecorderSourcesFailed, &[("error", error)])
            .unwrap(),
        format!("无法加载 Linux 录制来源：{error}")
    );
    assert_eq!(
        localizer("en").format(
            Message::RecorderSourcesFound,
            &[("count", "1"), ("displayServer", "X11")]
        ),
        Err(FormatError::UnknownArgument)
    );
}
