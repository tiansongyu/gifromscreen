//! Navigate real pages and simulate asynchronous native geometry acknowledgements.
use eframe::egui::{self, Vec2, ViewportCommand, vec2};
use gif_from_screen_localization::Message;

use crate::{AppView, GifFromScreenApp, MainWindowRestore, MainWindowSnapshot, landing_ui};
use landing_ui::WindowProfile;

struct Window {
    context: egui::Context,
    app: GifFromScreenApp,
    size: Vec2,
    maximized: bool,
}

impl Window {
    fn new(zoom: f32) -> Self {
        let context = egui::Context::default();
        crate::appearance::configure(&context);
        crate::preferences::fonts::install(&context);
        context.set_zoom_factor(zoom);
        let mut app = GifFromScreenApp::default();
        app.source_catalog_attempted = true;
        Self {
            context,
            app,
            size: landing_ui::INITIAL_WINDOW_SIZE,
            maximized: false,
        }
    }

    fn input(&self, events: Vec<egui::Event>) -> egui::RawInput {
        let zoom = self.context.zoom_factor();
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                self.size / zoom,
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.native_pixels_per_point = Some(1.0);
        viewport.maximized = Some(self.maximized);
        input
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.context.run(self.input(events), |context| {
            self.app.restore_main_window_if_requested(context);
            self.app.show_main_view(context);
        })
    }

    fn profile_frame(&mut self, profile: WindowProfile) -> egui::FullOutput {
        self.context.run(self.input(vec![]), |context| {
            self.app.window_sizing.update(context, profile);
        })
    }

    fn apply(&mut self, output: &egui::FullOutput) {
        for command in commands(output) {
            match command {
                ViewportCommand::InnerSize(size) => self.size = *size * self.context.zoom_factor(),
                ViewportCommand::Maximized(value) => self.maximized = *value,
                _ => {}
            }
        }
    }

    fn settle(&mut self) {
        for _ in 0..5 {
            let output = self.frame(vec![]);
            self.apply(&output);
        }
    }

    fn back(&mut self) -> egui::FullOutput {
        let output = self.frame(vec![]);
        let caption = self
            .app
            .language_settings
            .localizer()
            .text(Message::BackToHome);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == caption => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("visible back button");
        let events = |pressed| {
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        self.frame(events(true));
        self.frame(events(false))
    }
}

fn commands(output: &egui::FullOutput) -> &[ViewportCommand] {
    &output.viewport_output[&egui::ViewportId::ROOT].commands
}

#[test]
fn compact_entries_keep_primary_controls_visible_in_both_languages_at_every_zoom() {
    for tag in ["en", "zh"] {
        for zoom in [1.0, 1.5, 2.0] {
            for route in [AppView::CameraRecorder, AppView::Editor] {
                let mut window = Window::new(zoom);
                window.app.language_settings = crate::preferences::LanguageSettings::with_language(
                    gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
                );
                let localizer = window.app.language_settings.localizer();
                window.app.sync_ui_language(localizer);
                window.settle();
                window.app.open_landing_view(route);
                window.settle();
                let output = window.frame(vec![]);
                let captions = if route == AppView::CameraRecorder {
                    vec!["Camera recorder", "Settings", "Preview camera", "Record"]
                } else {
                    [
                        Message::EditorEmptyTitle,
                        Message::HomeOpenProject,
                        Message::HomeImportGif,
                        Message::HomeImportImage,
                        Message::HomeNewAnimation,
                    ]
                    .map(|message| localizer.text(message))
                    .to_vec()
                };
                for caption in captions {
                    let visible = output.shapes.iter().any(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == caption => {
                            shape.clip_rect.contains_rect(text.visual_bounding_rect())
                        }
                        _ => false,
                    });
                    assert!(
                        visible,
                        "{tag}/{route:?}: {caption:?} must fit at zoom {zoom}"
                    );
                }
            }
        }
    }
}

#[test]
fn small_home_opens_compact_entries_and_full_workbenches_at_every_zoom() {
    for zoom in [1.0, 1.5, 2.0] {
        for (route, expected) in [
            (AppView::ScreenRecorder, vec2(960.0, 680.0)),
            (AppView::CameraRecorder, vec2(640.0, 190.0 * zoom)),
            (AppView::BoardRecorder, vec2(960.0, 680.0)),
            (AppView::Editor, vec2(640.0, 210.0 * zoom)),
            (AppView::OpenProject, vec2(960.0, 680.0)),
            (AppView::ImportGif, vec2(960.0, 680.0)),
            (AppView::ImportImage, vec2(960.0, 680.0)),
            (AppView::ImportImageSequence, vec2(960.0, 680.0)),
            (AppView::ImportVideo, vec2(960.0, 680.0)),
            (AppView::NewBlankAnimation, vec2(960.0, 680.0)),
            (AppView::Automation, vec2(960.0, 680.0)),
        ] {
            let mut window = Window::new(zoom);
            window.settle();
            window.app.open_landing_view(route);
            window.settle();
            assert_eq!(window.size, expected, "{route:?}, zoom {zoom}");
            let output = window.frame(vec![]);
            assert!(
                !commands(&output)
                    .iter()
                    .any(|command| matches!(command, ViewportCommand::InnerSize(_)))
            );
            let output = window.back();
            window.apply(&output);
            window.settle();
            assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
        }
    }
}

#[test]
fn loading_a_project_expands_the_compact_editor_and_keeps_its_home_size() {
    use gif_from_screen_application::{
        BlankAnimationProjectOptions, create_blank_animation_project,
    };
    use gif_from_screen_domain::{DurationUs, FrameId, PhysicalSize, ProjectId, Rgba, UnixTimeMs};
    for zoom in [1.0, 1.5, 2.0] {
        let directory = tempfile::tempdir().unwrap();
        let project = create_blank_animation_project(
            directory.path().join("window-size.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(7101),
                frame_id: FrameId::from_u128(7102),
                app_version: "window-size-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(160, 96).unwrap(),
                background: Rgba {
                    red: 20,
                    green: 50,
                    blue: 90,
                    alpha: 255,
                },
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let mut window = Window::new(zoom);
        window.settle();
        window.app.open_landing_view(AppView::Editor);
        window.settle();
        assert_eq!(window.size, vec2(640.0, 210.0 * zoom));
        let mut workspace =
            crate::editor_workspace::EditorWorkspace::from_active(project, 20).unwrap();
        workspace.select_first().unwrap();
        window.app.editor_workspace = Some(workspace);
        window.settle();
        assert_eq!(window.size, vec2(960.0, 680.0));
        let output = window.back();
        window.apply(&output);
        window.settle();
        assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
        window.app.open_landing_view(AppView::Editor);
        window.settle();
        assert_eq!(window.size, vec2(960.0, 680.0));
    }
}

#[test]
fn camera_preview_expands_once_and_restores_setup_without_growing_home() {
    for zoom in [1.0, 1.5, 2.0] {
        let mut window = Window::new(zoom);
        window.settle();
        for _ in 0..3 {
            for (profile, expected) in [
                (WindowProfile::CameraSetup, vec2(640.0, 190.0 * zoom)),
                (WindowProfile::CameraPreview, vec2(800.0, 700.0)),
                (WindowProfile::CameraSetup, vec2(640.0, 190.0 * zoom)),
                (WindowProfile::Home, landing_ui::INITIAL_WINDOW_SIZE),
            ] {
                let output = window.profile_frame(profile);
                window.apply(&output);
                for _ in 0..3 {
                    let output = window.profile_frame(profile);
                    window.apply(&output);
                }
                assert_eq!(window.size, expected, "{profile:?}, zoom {zoom}");
                assert!(
                    !commands(&window.profile_frame(profile))
                        .iter()
                        .any(|command| matches!(command, ViewportCommand::InnerSize(_)))
                );
            }
        }
    }
}

#[test]
fn preview_profiles_preserve_manual_sizes_and_maximized_windows() {
    for maximized in [false, true] {
        let mut window = Window::new(1.0);
        window.settle();
        let output = window.profile_frame(WindowProfile::CameraSetup);
        window.apply(&output);
        let output = window.profile_frame(WindowProfile::CameraPreview);
        window.apply(&output);
        window.size = vec2(1200.0, 800.0);
        window.maximized = maximized;
        for profile in [
            WindowProfile::CameraSetup,
            WindowProfile::CameraPreview,
            WindowProfile::EmptyEditor,
            WindowProfile::Workbench,
        ] {
            let output = window.profile_frame(profile);
            assert!(!commands(&output).iter().any(|command| matches!(
                command,
                ViewportCommand::InnerSize(_) | ViewportCommand::Maximized(_)
            )));
            assert_eq!(window.size, vec2(1200.0, 800.0));
        }
    }
}

#[test]
fn cancelled_camera_expansion_replaces_its_pending_size_request() {
    let mut window = Window::new(1.5);
    window.settle();
    let output = window.profile_frame(WindowProfile::CameraSetup);
    window.apply(&output);
    let output = window.profile_frame(WindowProfile::CameraPreview);
    assert!(commands(&output).contains(&ViewportCommand::InnerSize(vec2(800.0, 700.0) / 1.5)));
    // Stop before the native system acknowledges the preview expansion.
    let output = window.profile_frame(WindowProfile::CameraSetup);
    assert!(
        commands(&output).contains(&ViewportCommand::InnerSize(vec2(640.0, 190.0 * 1.5) / 1.5))
    );
    window.apply(&output);
    let output = window.profile_frame(WindowProfile::Home);
    window.apply(&output);
    for _ in 0..3 {
        let output = window.profile_frame(WindowProfile::Home);
        window.apply(&output);
    }
    assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
}

#[test]
fn recording_restore_then_real_back_button_restores_home_without_growth_over_cycles() {
    for zoom in [1.0, 1.5, 2.0] {
        let mut window = Window::new(zoom);
        window.settle();
        for _ in 0..3 {
            window.app.open_landing_view(AppView::ScreenRecorder);
            window.settle();
            assert_eq!(window.size, vec2(960.0, 680.0));
            let scale = window.context.pixels_per_point();
            // This is the snapshot left by the real X11 recorder; its old work
            // minimum must be replaced before asking for the compact home size.
            window.app.main_window_snapshot = Some(MainWindowSnapshot {
                position: None,
                size: window.size / zoom * scale,
                maximized: Some(false),
                restore: MainWindowRestore::X11Geometry,
            });
            window.size = vec2(640.0, 40.0);
            window.app.restore_main_window = true;
            window.app.view = AppView::Editor;
            window.settle();
            let output = window.back();
            assert_eq!(window.app.view, AppView::Landing);
            assert!(commands(&output).contains(&ViewportCommand::MinInnerSize(
                landing_ui::MINIMUM_WINDOW_SIZE / zoom
            )));
            assert!(commands(&output).contains(&ViewportCommand::InnerSize(
                landing_ui::INITIAL_WINDOW_SIZE / zoom
            )));
            window.apply(&output);
            window.settle();
            assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
            assert!(
                !commands(&window.frame(vec![]))
                    .iter()
                    .any(|c| matches!(c, ViewportCommand::InnerSize(_)))
            );
        }
    }
}

#[test]
fn manual_home_size_is_preserved_across_all_work_routes_and_zoom_changes() {
    let mut window = Window::new(1.0);
    window.size = vec2(610.0, 230.0);
    window.settle();
    for route in [
        AppView::OpenProject,
        AppView::ImportGif,
        AppView::CameraRecorder,
        AppView::BoardRecorder,
        AppView::Editor,
    ] {
        window.app.open_landing_view(route);
        window.settle();
        window.context.set_zoom_factor(1.5);
        window.size = vec2(1100.0, 750.0);
        let output = window.back();
        window.apply(&output);
        window.settle();
        assert_eq!(window.size, vec2(610.0, 230.0));
    }
}

#[test]
fn home_resize_waits_for_delayed_recording_restore_before_replacing_its_minimum() {
    let mut window = Window::new(1.0);
    window.settle();
    window.app.open_landing_view(AppView::ScreenRecorder);
    window.settle();
    window.app.main_window_snapshot = Some(MainWindowSnapshot {
        position: None,
        size: window.size,
        maximized: Some(false),
        restore: MainWindowRestore::Wayland {
            pending: None,
            restoring: false,
            zoom_factor: 1.0,
        },
    });
    window.app.restore_main_window = true;
    window.app.view = AppView::Landing;
    window.size = vec2(720.0, 480.0);
    let output = window.frame(vec![]);
    assert!(commands(&output).contains(&ViewportCommand::InnerSize(vec2(960.0, 680.0))));
    assert!(
        !commands(&output).contains(&ViewportCommand::InnerSize(landing_ui::INITIAL_WINDOW_SIZE))
    );
    for _ in 0..3 {
        let output = window.frame(vec![]);
        assert!(
            !commands(&output)
                .iter()
                .any(|c| matches!(c, ViewportCommand::InnerSize(_)))
        );
    }
    window.size = vec2(960.0, 680.0); // Delayed compositor acknowledgement.
    let output = window.frame(vec![]);
    assert!(window.app.main_window_snapshot.is_none());
    assert!(
        commands(&output).contains(&ViewportCommand::InnerSize(landing_ui::INITIAL_WINDOW_SIZE))
    );
    window.apply(&output);
    window.settle();
    assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
}

#[test]
fn maximized_editor_unmaximizes_before_home_resize_and_quick_navigation_cancels_it() {
    let mut window = Window::new(1.0);
    window.settle();
    window.app.open_landing_view(AppView::Editor);
    window.settle();
    window.size = vec2(1920.0, 1080.0);
    window.maximized = true;
    let output = window.back();
    assert!(commands(&output).contains(&ViewportCommand::Maximized(false)));
    assert!(
        !commands(&output)
            .iter()
            .any(|c| matches!(c, ViewportCommand::InnerSize(_)))
    );
    window.maximized = false;
    let output = window.frame(vec![]);
    assert!(
        commands(&output).contains(&ViewportCommand::InnerSize(landing_ui::INITIAL_WINDOW_SIZE))
    );
    // Leave again before the resize arrives: never record the 1920x1080 work
    // extent as the home size, and do not keep issuing the old home request.
    window.app.open_landing_view(AppView::Editor);
    let output = window.frame(vec![]);
    assert!(
        !commands(&output)
            .iter()
            .any(|c| matches!(c, ViewportCommand::InnerSize(_)))
    );
    let output = window.back();
    window.apply(&output);
    window.settle();
    assert_eq!(window.size, landing_ui::INITIAL_WINDOW_SIZE);
}
