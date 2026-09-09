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
    assert_eq!(ALL_MESSAGES.len(), 848);
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
fn watermark_dimensions_paths_and_diagnostics_have_literal_named_parameters() {
    let path = "/home/用户/{width}/{height}/{path}.png";
    let arguments = [("width", "20"), ("height", "10"), ("path", path)];
    assert_eq!(
        Message::WatermarkAdded.parameters(),
        &["width", "height", "path"]
    );
    assert_eq!(
        localizer("en")
            .format(Message::WatermarkAdded, &arguments)
            .unwrap(),
        format!("Added 20×10 watermark {path} to the selected frame span.")
    );
    assert_eq!(
        localizer("zh")
            .format(Message::WatermarkAdded, &arguments)
            .unwrap(),
        format!("已将 20×10 水印 {path} 添加到选中帧范围。")
    );
    for tag in ["en", "zh"] {
        let language = localizer(tag);
        for message in [
            Message::WatermarkStartFailed,
            Message::WatermarkDecodeFailed,
            Message::WatermarkCommitFailed,
            Message::WatermarkInvalidDisplaySize,
        ] {
            let raw = "Watermark name is required. 原始错误 {error}/{path}\n";
            assert!(
                language
                    .format(message, &[("error", raw)])
                    .unwrap()
                    .contains(raw)
            );
            assert_eq!(
                language.format(message, &[("错误", raw)]),
                Err(FormatError::UnknownArgument)
            );
        }
        assert_eq!(
            language.format(
                Message::WatermarkAdded,
                &[("宽度", "20"), ("height", "10"), ("path", path)]
            ),
            Err(FormatError::UnknownArgument)
        );
    }
}

#[test]
fn saved_text_group_labels_preserve_names_counts_and_typed_validation_parameters() {
    let name = "用户 {layer}/{name}/{count} Text";
    for message in [
        Message::TextSavedFrame,
        Message::TextSavedFrames,
        Message::TextSavedItem,
        Message::TextSavedItems,
    ] {
        assert_eq!(message.parameters(), &["layer", "name", "count"]);
        for tag in ["en", "zh"] {
            let text = localizer(tag)
                .format(message, &[("layer", "65"), ("name", name), ("count", "3")])
                .unwrap();
            assert!(text.contains(name) && text.contains("65") && text.contains('3'));
        }
    }
    assert_eq!(
        localizer("en")
            .format(
                Message::TextSavedFrames,
                &[("layer", "65"), ("name", name), ("count", "3")]
            )
            .unwrap(),
        format!("Layer 65 · {name} · 3 frames")
    );
    for tag in ["en", "zh"] {
        for message in [Message::TextFontSizeRange, Message::TextDimensions] {
            assert!(
                localizer(tag)
                    .format(message, &[("minimum", "1"), ("maximum", "512")])
                    .unwrap()
                    .contains("512")
            );
            assert_eq!(
                localizer(tag).format(message, &[("最小值", "1"), ("maximum", "512")]),
                Err(FormatError::UnknownArgument)
            );
        }
        let raw = "Enter some text first. /用户/{error}/{path}";
        for message in [
            Message::TextStartFailed,
            Message::TextSaveFailed,
            Message::TextLoadFailed,
            Message::TextWorkerStartFailed,
        ] {
            assert!(
                localizer(tag)
                    .format(message, &[("error", raw)])
                    .unwrap()
                    .contains(raw)
            );
        }
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

#[test]
fn virtual_portal_chooser_labels_have_exact_english_and_explicit_fallback() {
    for (message, english, chinese) in [
        (
            Message::RecorderChoosePortalScreen,
            "Choose a screen with the system portal",
            "通过系统 Portal 选择显示器",
        ),
        (
            Message::RecorderChoosePortalWindow,
            "Choose a window with the system portal",
            "通过系统 Portal 选择窗口",
        ),
    ] {
        assert!(message.parameters().is_empty());
        assert_eq!(localizer("en").text(message), english);
        assert_eq!(localizer("zh").text(message), chinese);
        let fallback = localizer("fr").resolve(message);
        assert_eq!(fallback.text, english);
        assert_eq!(fallback.source, CatalogSource::EnglishFallback);
        assert_eq!(fallback.language_tag, "en");
    }
}

#[test]
fn catalog_ids_and_parameter_names_remain_machine_readable_not_translated() {
    for &message in ALL_MESSAGES {
        assert!(
            message
                .id()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        );
        for name in message.parameters() {
            assert!(!name.is_empty());
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            );
        }
    }
}

#[test]
fn shortcut_descriptions_and_diagnostic_arguments_are_never_key_lookups() {
    let trigger = "Ctrl+Shift+F7 / 用户 {action}";
    let error = "org.freedesktop.portal.Error {count}: /home/用户/{error}";
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        let action = localizer.text(Message::ShortcutsActionStartPause);
        assert_eq!(
            localizer
                .format(
                    Message::ShortcutsRegisteredBinding,
                    &[("trigger", trigger), ("action", action),]
                )
                .unwrap(),
            format!("{action} — {trigger}")
        );
        for message in [
            Message::ShortcutsRegistrationFailed,
            Message::ShortcutsInvalidBindings,
            Message::ShortcutsSettingsIoFailed,
            Message::ShortcutsSettingsUnavailable,
        ] {
            assert!(
                localizer
                    .format(message, &[("error", error)])
                    .unwrap()
                    .contains(error)
            );
        }
        assert_eq!(
            localizer.format(
                Message::ShortcutsRegisteredBinding,
                &[("action", action), ("description", trigger),]
            ),
            Err(FormatError::UnknownArgument)
        );
    }
}

#[test]
fn editor_named_counts_reorder_without_swapping_values_or_touching_ids() {
    let arguments = [("frames", "3"), ("snapshots", "2")];
    assert_eq!(
        localizer("en")
            .format(Message::EditorClipboardSummary, &arguments)
            .unwrap(),
        "Clipboard: 3 frame(s) in 2 snapshot(s)"
    );
    assert_eq!(
        localizer("zh")
            .format(Message::EditorClipboardSummary, &arguments)
            .unwrap(),
        "剪贴板：2 份快照，共 3 帧"
    );
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        let id = "用户-{frames}-42";
        let duration = "1.234s {id}";
        let output = localizer
            .format(
                Message::EditorClipboardEntry,
                &[("id", id), ("frames", "3"), ("duration", duration)],
            )
            .unwrap();
        assert!(output.starts_with(&format!("#{id} · ")));
        assert!(output.ends_with(duration));
        assert!(localizer.text(Message::EditorTimeRange).ends_with(") ms"));
        assert_eq!(
            localizer.format(
                Message::EditorFilmstripFrame,
                &[("帧号", "1"), ("duration", "125000"),]
            ),
            Err(FormatError::UnknownArgument)
        );
        assert!(
            localizer
                .format(
                    Message::EditorFilmstripFrame,
                    &[("number", "1"), ("duration", "125000"),]
                )
                .unwrap()
                .ends_with("125000 µs")
        );
    }
}

#[test]
fn recording_notice_identity_can_render_again_without_rewriting_raw_arguments() {
    let message = Message::RecorderSnapshotCaptured;
    let arguments = [("sequence", "7"), ("seconds", "0.125")];
    assert_eq!(
        localizer("en").format(message, &arguments).unwrap(),
        "Snapshot captured from native frame 7 at 0.125s."
    );
    assert_eq!(
        localizer("zh").format(message, &arguments).unwrap(),
        "已从时间为 0.125s 的原生帧 7 采集快照。"
    );
    let error = "PermissionDenied {path} {sequence}";
    let path = "/home/用户/{error}/Recording Paused.gfsproj";
    let arguments = [("error", error), ("path", path)];
    let before = arguments;
    for tag in ["en", "zh"] {
        let output = localizer(tag)
            .format(Message::RecorderFailedRecoverable, &arguments)
            .unwrap();
        assert!(output.contains(error));
        assert!(output.ends_with(path));
        assert_eq!(arguments, before);
        assert_eq!(
            localizer(tag).format(Message::RecorderSnapshotRejected, &[("error", error)]),
            Err(FormatError::UnknownArgument)
        );
        assert!(
            localizer(tag)
                .format(Message::RecorderSnapshotRejected, &[("reason", error)])
                .unwrap()
                .ends_with(error)
        );
    }
}

#[test]
fn editor_result_receipts_keep_operation_ids_and_literal_errors() {
    let operation = "SetFrameDuration";
    let error = "PermissionDenied: /home/用户/{operation}/原始 {error}";
    let arguments = [("error", error), ("operation", operation)];
    assert_eq!(
        localizer("en")
            .format(Message::EditorOperationFailed, &arguments)
            .unwrap(),
        format!("Editor {operation} failed: {error}")
    );
    assert_eq!(
        localizer("zh")
            .format(Message::EditorOperationFailed, &arguments)
            .unwrap(),
        format!("编辑器操作 {operation} 失败：{error}")
    );
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        assert_eq!(
            localizer.format(Message::EditorOperationFailed, &[("error", error)]),
            Err(FormatError::MissingArgument("operation"))
        );
        assert_eq!(
            localizer.format(
                Message::EditorOperationFailed,
                &[("operation", operation), ("reason", error)]
            ),
            Err(FormatError::UnknownArgument)
        );
        assert!(Message::EditorUndoCompleted.parameters().is_empty());
        assert!(Message::EditorRedoCompleted.parameters().is_empty());
    }
    assert_eq!(
        localizer("en").text(Message::EditorUndoCompleted),
        "Undid the previous edit."
    );
    assert_eq!(
        localizer("en").text(Message::EditorRedoCompleted),
        "Reapplied the edit."
    );
}

#[test]
fn export_report_keeps_selected_encoded_counts_and_user_path_distinct() {
    let path = "/home/用户/{bytes}/GIF {encoded_frames}.gif";
    let arguments = [
        ("path", path),
        ("bytes", "4096"),
        ("encoded_frames", "3"),
        ("selected_frames", "7"),
    ];
    assert_eq!(
        localizer("en")
            .format(Message::ExportCompletedReport, &arguments)
            .unwrap(),
        format!("Exported 7 selected frames as 3 GIF images (4096 bytes) to {path}")
    );
    assert_eq!(
        localizer("zh")
            .format(Message::ExportCompletedReport, &arguments)
            .unwrap(),
        format!("已将 7 个选中帧导出为 3 幅 GIF 图像（4096 字节），保存到 {path}")
    );
    assert_eq!(Message::ExportFinished.parameters(), ["frames", "path"]);
    assert_eq!(Message::ExportFailed.parameters(), ["reason"]);
    assert_eq!(
        localizer("zh").format(Message::ExportFailed, &[("error", "raw")]),
        Err(FormatError::UnknownArgument)
    );
}

#[test]
fn export_progress_uses_one_total_parameter_and_retains_technical_labels() {
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        let phase = localizer.text(Message::ExportPhaseRendering);
        let output = localizer
            .format(
                Message::ExportProgress,
                &[
                    ("phase", phase),
                    ("frames_rendered", "5"),
                    ("frames_encoded", "2"),
                    ("total_frames", "7"),
                ],
            )
            .unwrap();
        assert!(output.starts_with(phase));
        assert!(output.contains("5/7"));
        assert!(output.contains("2/7"));
        assert_eq!(localizer.text(Message::ExportQuantizerNeuQuant), "NeuQuant");
        assert_eq!(
            localizer.text(Message::ExportDitherFloydSteinberg),
            "Floyd–Steinberg"
        );
        assert_eq!(localizer.text(Message::ExportDitherBayer), "Bayer 4×4");
        assert_eq!(localizer.text(Message::PreviewZoomNative), "100%");
        assert_eq!(localizer.text(Message::CropFieldX), "X");
        assert_eq!(
            localizer.format(
                Message::ExportProgress,
                &[
                    ("phase", phase),
                    ("frames_rendered", "5"),
                    ("frames_encoded", "2"),
                    ("total_frames", "7"),
                    ("total_frames", "8"),
                ]
            ),
            Err(FormatError::DuplicateArgument("total_frames"))
        );
    }
}

#[test]
fn preset_names_and_crop_diagnostics_are_not_translation_templates() {
    let name = "我的预设 {name} #RRGGBB / 100%";
    let error = "PermissionDenied: /home/用户/{error}/原始诊断";
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        for message in [
            Message::ExportPresetLoaded,
            Message::ExportPresetSaved,
            Message::ExportPresetUpdated,
            Message::ExportPresetRenamed,
        ] {
            let output = localizer.format(message, &[("name", name)]).unwrap();
            assert!(output.contains(name));
            assert_eq!(
                localizer.format(message, &[("名称", name)]),
                Err(FormatError::UnknownArgument)
            );
        }
        for message in [
            Message::CropInvalidBounds,
            Message::CropOperationFailed,
            Message::CropStartFailed,
            Message::PreviewRenderFailed,
            Message::ExportPresetFailed,
        ] {
            assert!(
                localizer
                    .format(message, &[("error", error)])
                    .unwrap()
                    .ends_with(error)
            );
        }
        assert!(
            localizer
                .text(Message::ExportCustomPaletteHint)
                .contains("#RRGGBB")
        );
        assert_eq!(
            Message::PreviewRenderedSizes.parameters(),
            [
                "rendered_width",
                "rendered_height",
                "preview_width",
                "preview_height"
            ]
        );
    }
}

#[test]
fn advanced_effect_field_labels_are_parameterless_and_preserve_raw_parse_details() {
    for label in [
        Message::EffectFieldBlurRadius,
        Message::EffectFieldBlockSize,
        Message::EffectFieldTonePercent,
        Message::EffectFieldBorderTop,
        Message::EffectFieldBorderRight,
        Message::EffectFieldBorderBottom,
        Message::EffectFieldBorderLeft,
        Message::EffectFieldShadowBlur,
        Message::EffectFieldShadowX,
        Message::EffectFieldShadowY,
        Message::EffectFieldRegionX,
        Message::EffectFieldRegionY,
        Message::EffectFieldRegionWidth,
        Message::EffectFieldRegionHeight,
        Message::EffectFieldRed,
        Message::EffectFieldGreen,
        Message::EffectFieldBlue,
        Message::EffectFieldAlpha,
        Message::EffectFieldIndex,
    ] {
        assert!(label.parameters().is_empty());
    }
    let error = "invalid digit: {field} 用户输入 -1.25";
    assert_eq!(
        localizer("en")
            .format(
                Message::EditorInputInvalid,
                &[
                    (
                        "field",
                        localizer("en").text(Message::EffectFieldBlurRadius)
                    ),
                    ("error", error),
                ]
            )
            .unwrap(),
        format!("invalid blur radius: {error}")
    );
    assert_eq!(
        localizer("zh")
            .format(
                Message::EditorInputInvalid,
                &[
                    (
                        "field",
                        localizer("zh").text(Message::EffectFieldBlurRadius)
                    ),
                    ("error", error),
                ]
            )
            .unwrap(),
        format!("模糊半径无效：{error}")
    );
    assert_eq!(
        localizer("zh")
            .format(
                Message::EditorInputRequired,
                &[("field", localizer("zh").text(Message::EffectFieldShadowX)),]
            )
            .unwrap(),
        "请输入阴影 X 偏移。"
    );
    assert_eq!(
        localizer("zh").format(
            Message::EditorInputInvalid,
            &[("字段", "模糊半径"), ("error", error),]
        ),
        Err(FormatError::UnknownArgument)
    );
}

#[test]
fn layer_rows_keep_user_names_and_absolute_pagination_values_literal() {
    let name = "Screen / 用户 {kind} #{number}";
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        let kind = localizer.text(Message::EditorContentShape);
        let ownership = localizer.text(Message::EditorFrameOwned);
        let output = localizer
            .format(
                Message::EditorLayerRow,
                &[
                    ("number", "65"),
                    ("name", name),
                    ("kind", kind),
                    ("count", "3"),
                    ("ownership", ownership),
                ],
            )
            .unwrap();
        assert!(output.contains(name));
        assert!(output.contains(kind));
        assert!(output.ends_with(ownership));
    }
    let arguments = [
        ("start", "65"),
        ("end", "128"),
        ("total", "129"),
        ("page", "2"),
        ("pages", "3"),
    ];
    assert_eq!(
        localizer("en")
            .format(Message::EditorLayerPage, &arguments)
            .unwrap(),
        "Layers 65–128 of 129 · Page 2 / 3"
    );
    assert_eq!(
        localizer("zh")
            .format(Message::EditorLayerPage, &arguments)
            .unwrap(),
        "图层 65–128，共 129 个 · 第 2 / 3 页"
    );
}

#[test]
fn project_statistics_and_preserved_journal_do_not_localize_user_data() {
    let arguments = [
        ("number", "3"),
        ("start", "0.000001 s"),
        ("delay", "0.001234 s"),
    ];
    assert_eq!(
        localizer("en")
            .format(Message::EditorStatsCurrentValue, &arguments)
            .unwrap(),
        "#3 · start 0.000001 s · delay 0.001234 s"
    );
    assert_eq!(
        localizer("zh")
            .format(Message::EditorStatsCurrentValue, &arguments)
            .unwrap(),
        "#3 · 开始 0.000001 s · 延时 0.001234 s"
    );
    let path = "/home/用户/{path}/记录.journal";
    for tag in ["en", "zh"] {
        let localizer = localizer(tag);
        assert!(
            localizer
                .format(Message::EditorJournalPreserved, &[("path", path)])
                .unwrap()
                .contains(path)
        );
        assert!(
            localizer
                .format(Message::EffectBlurRange, &[("maximum", "128")])
                .unwrap()
                .contains("128")
        );
        assert!(
            localizer
                .format(Message::EditorDrawingTooManyPoints, &[("limit", "32768")])
                .unwrap()
                .contains("32768")
        );
        assert_eq!(
            localizer.format(
                Message::EditorDrawingTooManyPoints,
                &[("MAX_DRAWING_DRAFT_POINTS", "32768")]
            ),
            Err(FormatError::UnknownArgument)
        );
    }
}
