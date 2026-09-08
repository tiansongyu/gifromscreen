//! Real egui inputs and typed export outcomes; no display server or network.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use gif_from_screen_application::{
    ProjectExportSnapshot, ProjectGifExportError, ProjectGifExportOptions, ProjectGifExportReport,
};
use gif_from_screen_domain::{
    Canvas, CanvasBackground, ColorSpace, PhysicalSize, ProjectId, ProjectManifest,
    ProjectRevision, UnixTimeMs,
};
use gif_from_screen_gif::EncodeReport;
use gif_from_screen_localization::{Localizer, Message, find_language};
use gif_from_screen_project::ActiveProject;

use crate::{
    EditorExportAction, EditorExportSettings, ExportDitherChoice, ExportFrameScope,
    ExportLoopChoice, ExportPaletteChoice, ExportQuantizerChoice,
    editor_workspace::EditorWorkspace,
    export_job::{ExportJob, ExportJobError, ExportJobState},
    export_result_notice, show_export_configuration, show_export_panel,
};

fn language(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

fn context() -> egui::Context {
    let context = egui::Context::default();
    crate::preferences::fonts::install(&context);
    context.style_mut(|style| style.animation_time = 0.0);
    context
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 1400.0),
        )),
        events,
        focused: true,
        ..Default::default()
    }
}

fn pointer(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn key(key: egui::Key, modifiers: egui::Modifiers, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

fn configuration_frame(
    context: &egui::Context,
    settings: &mut EditorExportSettings,
    output: &mut String,
    tag: &str,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run(input(events), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            show_export_configuration(ui, output, settings, 7, language(tag));
        });
    })
}

fn base_settings() -> EditorExportSettings {
    EditorExportSettings {
        frame_scope: ExportFrameScope::Selected,
        max_colors: 256,
        palette: ExportPaletteChoice::Global,
        quantizer: ExportQuantizerChoice::Custom,
        custom_palette_text: "#000000, #FFFFFF\n#123456".into(),
        custom_transparency_enabled: true,
        custom_transparent_index: 0,
        dither: ExportDitherChoice::BlueNoise,
        delta: true,
        alpha_threshold: 123,
        loop_choice: ExportLoopChoice::Finite,
        finite_loop_count: 17,
        overwrite: true,
    }
}

fn configuration_cases() -> Vec<EditorExportSettings> {
    let mut cases = Vec::new();
    for palette in [ExportPaletteChoice::Local, ExportPaletteChoice::Global] {
        for loop_choice in [ExportLoopChoice::Infinite, ExportLoopChoice::Finite] {
            for frame_scope in [ExportFrameScope::All, ExportFrameScope::Selected] {
                for custom_transparency_enabled in [false, true] {
                    cases.push(EditorExportSettings {
                        frame_scope,
                        palette,
                        custom_transparency_enabled,
                        loop_choice,
                        ..base_settings()
                    });
                }
            }
        }
    }
    for quantizer in [
        ExportQuantizerChoice::MedianCut,
        ExportQuantizerChoice::Octree,
        ExportQuantizerChoice::Wu,
        ExportQuantizerChoice::Grayscale,
        ExportQuantizerChoice::MostUsed,
        ExportQuantizerChoice::NeuQuant,
        ExportQuantizerChoice::WebSafe216,
        ExportQuantizerChoice::Monochrome,
        ExportQuantizerChoice::Windows16,
        ExportQuantizerChoice::Custom,
    ] {
        cases.push(EditorExportSettings {
            quantizer,
            ..base_settings()
        });
    }
    for dither in [
        ExportDitherChoice::None,
        ExportDitherChoice::Bayer,
        ExportDitherChoice::Dotted,
        ExportDitherChoice::BlueNoise,
        ExportDitherChoice::InterleavedNoise,
        ExportDitherChoice::FloydSteinberg,
        ExportDitherChoice::Atkinson,
        ExportDitherChoice::Burkes,
        ExportDitherChoice::Sierra,
        ExportDitherChoice::SierraLite,
        ExportDitherChoice::TwoRowSierra,
        ExportDitherChoice::JarvisJudiceNinke,
        ExportDitherChoice::Stucki,
        ExportDitherChoice::StevensonArce,
    ] {
        cases.push(EditorExportSettings {
            dither,
            ..base_settings()
        });
    }
    cases
}

#[test]
fn all_export_choices_survive_english_chinese_roundtrip_without_input() {
    let context = context();
    let original_path = "/home/用户/{path}/GIF 导出.gif";
    let cases = configuration_cases();
    assert_eq!(cases.len(), 40);
    for original in cases {
        let mut settings = original.clone();
        let mut output = original_path.to_owned();
        for tag in ["en", "zh", "en"] {
            configuration_frame(&context, &mut settings, &mut output, tag, Vec::new());
            assert_eq!(settings, original, "language={tag}");
            assert_eq!(output, original_path);
        }
    }
    let mut settings = base_settings();
    settings.custom_palette_text = "  #RRGGBB {palette} 用户原文\n".into();
    let before = settings.clone();
    let mut output = original_path.to_owned();
    for tag in ["zh", "en"] {
        configuration_frame(&context, &mut settings, &mut output, tag, Vec::new());
        assert_eq!(settings, before);
        assert_eq!(output, original_path);
    }
}

fn hit_labels(settings: &EditorExportSettings, localizer: Localizer) -> Vec<String> {
    let mut labels = vec![
        "/tmp/用户 {path}.gif".to_owned(),
        "256".into(),
        "123".into(),
        localizer.text(Message::ExportScopeAll).into(),
        localizer
            .format(Message::ExportScopeSelectedCount, &[("count", "7")])
            .unwrap(),
        localizer
            .text(match settings.palette {
                ExportPaletteChoice::Local => Message::ExportPaletteLocal,
                ExportPaletteChoice::Global => Message::ExportPaletteGlobal,
            })
            .into(),
        crate::export_quantizer_label(settings.quantizer, localizer).into(),
        crate::export_dither_label(settings.dither, localizer).into(),
        localizer.text(Message::ExportLoopInfinite).into(),
        localizer.text(Message::ExportLoopFinite).into(),
        localizer.text(Message::ExportChangedRectangles).into(),
        localizer.text(Message::ExportOverwriteOutput).into(),
    ];
    if settings.quantizer == ExportQuantizerChoice::Custom {
        labels.push(localizer.text(Message::ExportTransparentIndex).into());
        if settings.custom_transparency_enabled {
            labels.push("0".into());
        }
    }
    if settings.loop_choice == ExportLoopChoice::Finite {
        labels.push("17".into());
    }
    labels
}

fn visible_position(output: &egui::FullOutput, label: &str, whole: bool) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape
                && text.galley.text() == label
            {
                let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                let visible = rect.intersect(shape.clip_rect);
                (visible.is_positive() && (!whole || shape.clip_rect.contains_rect(rect)))
                    .then_some(visible.center())
            } else {
                None
            }
        })
        .next_back()
        .unwrap_or_else(|| panic!("missing visible label {label:?}"))
}

fn hit_ids(tag: &str, mut settings: EditorExportSettings) -> Vec<Vec<egui::Id>> {
    let context = context();
    let original = settings.clone();
    let mut output = "/tmp/用户 {path}.gif".to_owned();
    for _ in 0..2 {
        configuration_frame(&context, &mut settings, &mut output, tag, Vec::new());
    }
    let mut ids = Vec::new();
    for label in hit_labels(&settings, language(tag)) {
        let frame = configuration_frame(&context, &mut settings, &mut output, tag, Vec::new());
        let position = visible_position(&frame, &label, false);
        configuration_frame(
            &context,
            &mut settings,
            &mut output,
            tag,
            vec![egui::Event::PointerMoved(position)],
        );
        let mut hovered: Vec<_> =
            context.interaction_snapshot(|state| state.hovered.iter().copied().collect());
        assert!(!hovered.is_empty(), "no actual widget hit for {label:?}");
        hovered.sort_by_key(egui::Id::value);
        for id in &hovered {
            assert!(context.read_response(*id).is_some());
        }
        ids.push(hovered);
    }
    assert!(ids.len() >= 12);
    assert_eq!(settings, original);
    assert_eq!(output, "/tmp/用户 {path}.gif");
    ids
}

#[test]
fn actual_pointer_hit_ids_are_language_independent_for_conditional_controls() {
    for quantizer in [
        ExportQuantizerChoice::Custom,
        ExportQuantizerChoice::WebSafe216,
    ] {
        for loop_choice in [ExportLoopChoice::Infinite, ExportLoopChoice::Finite] {
            for custom_transparency_enabled in [false, true] {
                let settings = EditorExportSettings {
                    quantizer,
                    custom_transparency_enabled,
                    loop_choice,
                    ..base_settings()
                };
                assert_eq!(hit_ids("en", settings.clone()), hit_ids("zh", settings));
            }
        }
    }
}

struct Panel {
    _directory: tempfile::TempDir,
    workspace: EditorWorkspace,
    context: egui::Context,
    output: String,
    settings: EditorExportSettings,
    job: ExportJob,
    mutation_active: bool,
}

impl Panel {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "用户 {name}",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(1, 1).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let project =
            ActiveProject::create(directory.path().join("project.gfsproj"), manifest).unwrap();
        Self {
            output: directory
                .path()
                .join("用户 {path}.gif")
                .display()
                .to_string(),
            _directory: directory,
            workspace: EditorWorkspace::from_active(project, 16).unwrap(),
            context: context(),
            settings: base_settings(),
            job: ExportJob::default(),
            mutation_active: false,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (egui::FullOutput, EditorExportAction) {
        let mut action = EditorExportAction::None;
        let output = self.context.run(input(events), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                action = show_export_panel(
                    ui,
                    &mut self.output,
                    &mut self.settings,
                    &self.job,
                    &mut self.workspace,
                    self.mutation_active,
                    language("zh"),
                );
            });
        });
        (output, action)
    }

    fn label_position(&mut self, label: &str) -> egui::Pos2 {
        self.frame(Vec::new());
        let output = self.frame(Vec::new()).0;
        visible_position(&output, label, true)
    }

    fn click(&mut self, label: &str) -> EditorExportAction {
        let position = self.label_position(label);
        assert_eq!(
            self.frame(pointer(position, true)).1,
            EditorExportAction::None
        );
        self.frame(pointer(position, false)).1
    }

    fn try_edit_path(&mut self) {
        let frame = self.frame(Vec::new()).0;
        let position = visible_position(&frame, &self.output, false);
        self.frame(pointer(position, true));
        self.frame(pointer(position, false));
        self.frame(vec![
            key(
                egui::Key::A,
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                true,
            ),
            egui::Event::Text("should-not-replace.gif".into()),
            key(
                egui::Key::A,
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                false,
            ),
        ]);
    }
}

#[test]
fn chinese_export_click_returns_start_but_editor_mutation_disables_export_and_edits() {
    let mut panel = Panel::new();
    let settings = panel.settings.clone();
    let path = panel.output.clone();
    let project = panel.workspace.manifest().clone();
    assert_eq!(panel.click("导出 GIF"), EditorExportAction::Start);
    assert_eq!(panel.frame(Vec::new()).1, EditorExportAction::None);
    panel.mutation_active = true;
    assert_eq!(panel.click("导出 GIF"), EditorExportAction::None);
    panel.try_edit_path();
    panel.click(language("zh").text(Message::ExportOverwriteOutput));
    assert_eq!(panel.settings, settings);
    assert_eq!(panel.output, path);
    assert_eq!(panel.workspace.manifest(), &project);
    assert_eq!(panel.job.state(), ExportJobState::Idle);
}

#[test]
fn public_job_running_and_cancelling_keep_configuration_immutable() {
    let mut panel = Panel::new();
    let settings = panel.settings.clone();
    let path = panel.output.clone();
    panel
        .job
        .start(
            ProjectExportSnapshot::from_active(panel.workspace.active_project()),
            PathBuf::from(&path),
            ProjectGifExportOptions::default(),
        )
        .unwrap();
    // The actual worker has no frames and will finish without producing a GIF.
    // Until drain receives that result, the real public lifecycle stays Running.
    assert_eq!(panel.job.state(), ExportJobState::Running);
    panel.try_edit_path();
    panel.click(language("zh").text(Message::ExportOverwriteOutput));
    assert_eq!(panel.click("取消导出"), EditorExportAction::Cancel);
    assert!(panel.job.cancel());
    assert_eq!(panel.click("正在取消…"), EditorExportAction::None);
    assert_eq!(panel.settings, settings);
    assert_eq!(panel.output, path);
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    while panel.job.state() != ExportJobState::Finished {
        panel.job.drain();
        assert!(
            Instant::now() < deadline,
            "owned export worker did not finish"
        );
        std::thread::yield_now();
    }
    assert!(panel.job.take_result().unwrap().is_err());
    assert!(!PathBuf::from(path).exists());
}

fn report(path: PathBuf) -> ProjectGifExportReport {
    ProjectGifExportReport {
        project_id: ProjectId::from_u128(1),
        revision: ProjectRevision::new(2),
        selected_frames: 7,
        encoding: EncodeReport {
            input_frames: 7,
            encoded_frames: 3,
            duplicate_frames_merged: 4,
            ..EncodeReport::default()
        },
        output_path: path,
        bytes_written: 4096,
    }
}

#[test]
fn successful_export_receipt_keeps_counts_and_path_and_can_render_later_in_chinese() {
    let path = PathBuf::from("/home/用户/{bytes}/GIF export cancelled. {encoded_frames}.gif");
    let notice = export_result_notice(Ok(report(path.clone())));
    assert_eq!(notice.message_id(), Some(Message::ExportCompletedReport));
    assert_eq!(
        notice.render(language("en")),
        format!(
            "Exported 7 selected frames as 3 GIF images (4096 bytes) to {}",
            path.display()
        )
    );
    assert_eq!(
        notice.render(language("zh")),
        format!(
            "已将 7 个选中帧导出为 3 幅 GIF 图像（4096 字节），保存到 {}",
            path.display()
        )
    );
    assert_eq!(notice.message_id(), Some(Message::ExportCompletedReport));
}

#[test]
fn only_the_cancelled_error_variant_produces_the_normal_cancel_receipt() {
    let cancelled = export_result_notice(Err(ProjectGifExportError::Cancelled.into()));
    assert_eq!(cancelled.message_id(), Some(Message::ExportCancelled));
    for tag in ["en", "zh"] {
        assert_eq!(
            cancelled.render(language(tag)),
            language(tag).text(Message::ExportCancelled)
        );
    }
    for error in [
        ExportJobError::WorkerExited,
        ProjectGifExportError::Io {
            operation: "write",
            path: PathBuf::from("/tmp/用户/{reason}.gif"),
            source: std::io::Error::other("GIF export cancelled. {reason} 原始错误"),
        }
        .into(),
    ] {
        let raw = error.to_string();
        let failed = export_result_notice(Err(error));
        assert_eq!(failed.message_id(), Some(Message::ExportFailed));
        for tag in ["en", "zh"] {
            assert_eq!(
                failed.render(language(tag)),
                language(tag)
                    .format(Message::ExportFailed, &[("reason", &raw)])
                    .unwrap()
            );
        }
    }
}

fn assert_notice(notice: &crate::ui_notice::Notice, message: Message, arguments: &[(&str, &str)]) {
    assert_eq!(notice.message_id(), Some(message));
    for tag in ["en", "zh", "en"] {
        assert_eq!(
            notice.render(language(tag)),
            language(tag).format(message, arguments).unwrap()
        );
    }
}

#[test]
fn real_selection_validation_is_typed_without_changing_frame_identity_or_order() {
    use gif_from_screen_domain::FrameId;
    use std::collections::BTreeSet;

    let selected = BTreeSet::new();
    assert_notice(
        &crate::resolve_export_selection(ExportFrameScope::All, &[], &selected).unwrap_err(),
        Message::ExportNoFrames,
        &[],
    );
    let frames = [
        FrameId::from_u128(2),
        FrameId::from_u128(1),
        FrameId::from_u128(3),
    ];
    assert_notice(
        &crate::resolve_export_selection(ExportFrameScope::Selected, &frames, &selected)
            .unwrap_err(),
        Message::ExportNoSelectedFrames,
        &[],
    );
    let selected = [frames[1], frames[0]].into_iter().collect();
    assert_eq!(
        crate::resolve_export_selection(ExportFrameScope::Selected, &frames, &selected).unwrap(),
        crate::ProjectFrameSelection::Ordered(vec![frames[0], frames[1]])
    );
    assert_eq!(
        crate::resolve_export_selection(ExportFrameScope::All, &frames, &selected).unwrap(),
        crate::ProjectFrameSelection::All
    );
    assert_eq!(
        frames,
        [
            FrameId::from_u128(2),
            FrameId::from_u128(1),
            FrameId::from_u128(3)
        ]
    );
}

#[test]
fn real_encoder_option_validation_keeps_numeric_arguments_and_raw_parser_errors() {
    let failure = |settings: &EditorExportSettings| {
        crate::build_project_export_options(settings, crate::ProjectFrameSelection::All)
            .unwrap_err()
    };
    for max_colors in [0, 1, 257] {
        let settings = EditorExportSettings {
            max_colors,
            ..base_settings()
        };
        assert_notice(&failure(&settings), Message::ExportColorsRange, &[]);
        assert_eq!(settings.max_colors, max_colors);
    }
    let mut custom = base_settings();
    custom.custom_palette_text = "{error} 用户输入".into();
    let before = custom.clone();
    let raw = crate::parse_custom_palette(&custom.custom_palette_text, Some(0))
        .unwrap_err()
        .to_string();
    assert_notice(
        &failure(&custom),
        Message::ExportInvalidCustomPalette,
        &[("error", &raw)],
    );
    assert_eq!(custom, before);
    let custom = EditorExportSettings {
        max_colors: 2,
        ..base_settings()
    };
    assert_notice(
        &failure(&custom),
        Message::ExportCustomPaletteTooLarge,
        &[("count", "3"), ("maximum", "2")],
    );
    for (quantizer, max_colors, required) in [
        (ExportQuantizerChoice::Monochrome, 2, "3"),
        (ExportQuantizerChoice::Windows16, 16, "17"),
        (ExportQuantizerChoice::WebSafe216, 216, "217"),
    ] {
        let settings = EditorExportSettings {
            max_colors,
            quantizer,
            ..base_settings()
        };
        assert_notice(
            &failure(&settings),
            Message::ExportFixedPaletteRequired,
            &[("required", required)],
        );
        assert_eq!(settings.max_colors, max_colors);
    }
    let settings = EditorExportSettings {
        finite_loop_count: 0,
        ..base_settings()
    };
    assert_notice(&failure(&settings), Message::ExportFiniteLoopMinimum, &[]);
}

#[test]
fn real_output_validation_preserves_existing_trim_extension_and_literal_path_rules() {
    for output in ["", "   ", "/"] {
        assert_notice(
            &crate::validate_export_output(output).unwrap_err(),
            Message::ExportOutputFileRequired,
            &[],
        );
    }
    for output in ["用户.png", "用户", "{path}.gif.txt"] {
        assert_notice(
            &crate::validate_export_output(output).unwrap_err(),
            Message::ExportOutputGifExtension,
            &[],
        );
    }
    let output = "  /tmp/用户 {path}/{error}.GIF  ".to_owned();
    let before = output.clone();
    assert_eq!(
        crate::validate_export_output(&output).unwrap(),
        PathBuf::from("/tmp/用户 {path}/{error}.GIF")
    );
    assert_eq!(output, before);
}
