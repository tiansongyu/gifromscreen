use super::*;

#[test]
fn default_uses_injected_machine_language_and_explicit_choice_wins() {
    let mut settings = LanguageSettings {
        environment: [None, None, Some("zh_CN.UTF-8".to_owned()), None],
        ..LanguageSettings::default()
    };
    assert_eq!(settings.localizer().requested_language().tag, "zh");
    settings.select(LanguagePreference::explicit("en").unwrap());
    assert_eq!(settings.localizer().requested_language().tag, "en");
    settings.select(LanguagePreference::System);
    assert_eq!(settings.localizer().requested_language().tag, "zh");
}

#[test]
fn missing_catalog_is_kept_in_the_choice_but_not_claimed_as_translated() {
    let mut settings = LanguageSettings::default();
    settings.select(LanguagePreference::explicit("ar").unwrap());
    assert_eq!(settings.localizer().coverage().translated, 0);
    assert_eq!(
        settings
            .localizer()
            .resolve(Message::HomeTitle)
            .language_tag,
        "en"
    );
    assert_eq!(
        choice_label(&settings.preferences.language, settings.localizer()),
        "Arabic [ar]"
    );
    settings.select(LanguagePreference::explicit("zz-ZZ").unwrap());
    assert_eq!(
        choice_label(&settings.preferences.language, settings.localizer()),
        "zz-ZZ"
    );
}

#[test]
fn language_window_keeps_its_identity_across_language_switches() {
    let context = egui::Context::default();
    fonts::install(&context);
    let mut settings = LanguageSettings {
        open: true,
        ..LanguageSettings::default()
    };
    for preference in [
        LanguagePreference::System,
        LanguagePreference::explicit("zh").unwrap(),
    ] {
        settings.select(preference);
        let output = context.run(egui::RawInput::default(), |context| settings.show(context));
        assert!(!output.shapes.is_empty());
        assert!(settings.open);
        assert!(context.memory(|memory| {
            memory
                .area_rect(egui::Id::new("application-language-settings"))
                .is_some()
        }));
    }
}

#[test]
fn application_close_waits_for_a_queued_language_save() {
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/preferences.json"));
    let mut settings = LanguageSettings {
        io: SettingsIo::new(Ok(store.clone())),
        ..LanguageSettings::default()
    };
    let context = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    while settings.is_active() {
        settings.poll(&context);
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    settings.select(LanguagePreference::explicit("zh").unwrap());
    let mut app = crate::GifFromScreenApp::default();
    app.language_settings = settings;
    let mut closing = egui::RawInput::default();
    closing
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    let output = context.run(closing, |context| app.handle_worker_shutdown(context));
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    loop {
        let output = context.run(egui::RawInput::default(), |context| {
            app.language_settings.poll(context);
            app.receive_background_messages(context);
            app.poll_recorder_shortcuts(context);
            app.handle_worker_shutdown(context);
        });
        if output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::Close)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shutdown: {:?}, language: {}, library: {}, shortcuts: {}, auto load: {}, source: {}",
            app.shutdown,
            app.language_settings.is_active(),
            app.project_library.is_active(),
            app.shortcut_tool.is_active(),
            app.auto_tasks.is_loading(),
            app.source_workers_active()
        );
        std::thread::yield_now();
    }
    assert_eq!(
        store.load().unwrap().preferences.language,
        LanguagePreference::explicit("zh").unwrap()
    );
}

#[test]
fn language_window_text_layout_audit_after_resize_and_font_enlargement() {
    let context = egui::Context::default();
    crate::appearance::configure(&context);
    fonts::install(&context);
    context.style_mut(|style| {
        style.animation_time = 0.0;
        for font in style.text_styles.values_mut() {
            font.size *= 1.5;
        }
    });
    let mut settings = LanguageSettings {
        open: true,
        ..LanguageSettings::default()
    };
    for (width, tag) in [(960.0, "en"), (440.0, "zh"), (320.0, "en"), (960.0, "zh")] {
        settings.select(LanguagePreference::explicit(tag).unwrap());
        let mut output = None;
        for _ in 0..8 {
            output = Some(context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 480.0),
                    )),
                    ..Default::default()
                },
                |context| settings.show(context),
            ));
        }
        let issues = crate::editor_shell_layout_tests::text_layout_issues(&output.unwrap());
        assert!(issues.is_empty(), "{width}/{tag}: {}", issues.join("\n"));
    }
}
