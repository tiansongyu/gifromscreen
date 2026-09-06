use super::*;
use gif_from_screen_capture::{CaptureSession, CaptureSessionState, FramePoll};
use gif_from_screen_workflow::{RecordingControl, RecordingFrameSink, RecordingFrameSinkError};

fn controller(context: &egui::Context) -> WaylandCropController {
    WaylandCropController {
        texture: context.load_texture(
            "wayland-controller-test",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        source_size: gif_from_screen_capture::PhysicalSize::new(640, 480).unwrap(),
        region: PhysicalRect::new(10, 20, 100, 80).unwrap(),
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    }
}

fn preparation_fixture(font_scale: f32) -> (egui::Context, GifFromScreenApp) {
    let context = egui::Context::default();
    context.style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size *= font_scale;
        }
    });
    let mut app = GifFromScreenApp::default();
    app.view = AppView::ScreenRecorder;
    app.settings.region_x = 41;
    app.settings.region_y = 53;
    app.settings.region_width = 211;
    app.settings.region_height = 173;
    app.wayland_frozen_preview = Some(WaylandFrozenPreview {
        texture: context.load_texture(
            "preparation-layout",
            egui::ColorImage::filled([692, 509], egui::Color32::BLUE),
            egui::TextureOptions::NEAREST,
        ),
        source_size: gif_from_screen_capture::PhysicalSize::new(692, 509).unwrap(),
        selection: PhysicalRect::new(10, 20, 100, 80).unwrap(),
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    });
    (context, app)
}

fn preparation_frame(
    context: &egui::Context,
    app: &mut GifFromScreenApp,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..egui::RawInput::default()
        },
        |context| {
            app.show_app_header(context);
            egui::CentralPanel::default().show(context, |ui| {
                let scroll_id = ui.make_persistent_id(egui::Id::new("wayland-preparation-content"));
                context.data_mut(|data| {
                    data.insert_temp(egui::Id::new("preparation-test-scroll-id"), scroll_id);
                });
                app.show_wayland_preparation(ui);
            });
        },
    )
}

fn preparation_text_rect(output: &egui::FullOutput, label: &str) -> (egui::Rect, egui::Rect) {
    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, label).map(|rect| (rect, clipped.clip_rect)))
        .unwrap_or_else(|| panic!("missing preparation control {label}"))
}

fn preparation_image_rect(output: &egui::FullOutput, texture: egui::TextureId) -> egui::Rect {
    fn find(shape: &egui::Shape, texture: egui::TextureId) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
            egui::Shape::Rect(rect)
                if rect
                    .brush
                    .as_ref()
                    .is_some_and(|brush| brush.fill_texture_id == texture) =>
            {
                Some(rect.rect)
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, texture)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, texture))
        .expect("preview image mesh")
}

#[test]
fn prepared_page_controls_have_visible_hit_targets_in_small_and_large_font_viewports() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            for label in [
                "Open source-local recorder controller",
                "Cancel preparation",
                "Apply exact region",
                "41",
                "53",
                "211",
                "173",
            ] {
                check_preparation_control_hit(size, font_scale, label);
            }
        }
    }
}

fn check_preparation_control_hit(size: egui::Vec2, font_scale: f32, label: &str) {
    let (context, mut app) = preparation_fixture(font_scale);
    preparation_frame(&context, &mut app, size, Vec::new());
    let output = preparation_frame(&context, &mut app, size, Vec::new());
    let (text, clip) = preparation_text_rect(&output, label);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    assert!(
        viewport.contains_rect(text),
        "{label}: {text:?} outside {size:?}, font {font_scale}"
    );
    assert!(
        clip.contains_rect(text),
        "{label}: control is clipped, font {font_scale}"
    );
    let pos = text.center();
    for pressed in [true, false] {
        preparation_frame(
            &context,
            &mut app,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let clicked = context
        .interaction_snapshot(|snapshot| snapshot.clicked)
        .expect("visible control must receive the pointer click");
    let response = context.read_response(clicked).unwrap();
    assert!(
        response.clicked() && response.rect.contains(pos),
        "{label} must have a real clickable target"
    );
    assert!(
        viewport.contains_rect(response.interact_rect),
        "{label} hit target must fit viewport"
    );
    if label == "Apply exact region" {
        assert_eq!(
            app.wayland_frozen_preview.as_ref().unwrap().selection,
            PhysicalRect::new(41, 53, 211, 173).unwrap()
        );
    } else if label == "Open source-local recorder controller" {
        assert_eq!(
            app.notice.as_deref(),
            Some("The Wayland source is not ready yet."),
            "the synthetic preview has no portal session, but the real Open handler must be reached"
        );
    }
}

#[test]
fn prepared_page_preview_fits_available_space_without_changing_aspect() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            let (context, mut app) = preparation_fixture(font_scale);
            let texture = app.wayland_frozen_preview.as_ref().unwrap().texture.id();
            preparation_frame(&context, &mut app, size, Vec::new());
            let before = preparation_frame(&context, &mut app, size, Vec::new());
            let initial = preparation_image_rect(&before, texture);
            assert!((initial.aspect_ratio() - 692.0 / 509.0).abs() < 0.001);
            assert!(initial.is_positive());
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(initial),
                "preview should fit rather than require scrolling at {size:?}, font {font_scale}"
            );
        }
    }
}

#[test]
fn prepared_page_overflow_scrolls_without_hiding_top_actions() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            let (context, mut app) = preparation_fixture(font_scale);
            app.notice = Some("Diagnostic detail must remain accessible.\n".repeat(40));
            preparation_frame(&context, &mut app, size, Vec::new());
            let before = preparation_frame(&context, &mut app, size, Vec::new());
            let scroll_id = context
                .data(|data| data.get_temp::<egui::Id>(egui::Id::new("preparation-test-scroll-id")))
                .unwrap();
            assert!(
                egui::scroll_area::State::load(&context, scroll_id)
                    .unwrap()
                    .offset
                    .y
                    .abs()
                    < f32::EPSILON
            );
            let after = preparation_frame(
                &context,
                &mut app,
                size,
                vec![
                    egui::Event::PointerMoved(egui::pos2(size.x / 2.0, size.y - 30.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -1000.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert!(
                egui::scroll_area::State::load(&context, scroll_id)
                    .unwrap()
                    .offset
                    .y
                    > 0.0,
                "overflowing body must scroll at {size:?}, font {font_scale}"
            );
            for label in [
                "Open source-local recorder controller",
                "Cancel preparation",
            ] {
                assert_eq!(
                    preparation_text_rect(&before, label).0,
                    preparation_text_rect(&after, label).0,
                    "{label} must stay above the scrolling body"
                );
            }
        }
    }
}

#[test]
fn controller_explains_source_specific_visibility_limits_without_font_dependent_arrows() {
    fn labels(shape: &egui::Shape, output: &mut String) {
        match shape {
            egui::Shape::Text(text) => output.push_str(text.galley.text()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| labels(shape, output)),
            _ => {}
        }
    }
    for monitor_source in [false, true] {
        let context = egui::Context::default();
        let mut controller = controller(&context);
        let output = context.run(egui::RawInput::default(), |context| {
            draw_wayland_crop_controller(
                context,
                RecorderStage::Ready,
                None,
                &mut controller,
                false,
                None,
                monitor_source,
            );
        });
        let mut text = String::new();
        for clipped in &output.shapes {
            labels(&clipped.shape, &mut text);
        }
        assert_eq!(
            text.contains("Monitor capture includes this controller"),
            monitor_source
        );
        assert_eq!(
            text.contains("Keep the selected window visible"),
            !monitor_source
        );
        assert!(
            !text.contains(['←', '→', '↑', '↓']),
            "nudge arrows must be painted, not missing-font glyphs"
        );
    }
}

#[test]
fn wayland_handoff_accepts_local_extent_when_desktop_window_positions_are_private() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    let prepared = controller(&context);
    let expected_region = prepared.region;
    app.wayland_frozen_preview = Some(WaylandFrozenPreview {
        texture: prepared.texture,
        source_size: prepared.source_size,
        selection: prepared.region,
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    });
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(960.0, 640.0),
        )),
        ..egui::RawInput::default()
    };
    let output =
        context.run(input, |context| {
            assert!(context.input(|input| input.viewport().inner_rect.is_none()
                && input.viewport().outer_rect.is_none()));
            app.enter_wayland_crop_controller(context).unwrap();
        });
    assert_eq!(
        app.wayland_crop_controller.as_ref().unwrap().region,
        expected_region
    );
    assert!(app.wayland_frozen_preview.is_none());
    let snapshot = app.main_window_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.size, egui::vec2(960.0, 640.0));
    assert!(snapshot.position.is_none());
    assert!(!snapshot.restore_geometry);
    assert!(
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .all(|command| !matches!(
                command,
                egui::ViewportCommand::Visible(_)
                    | egui::ViewportCommand::InnerSize(_)
                    | egui::ViewportCommand::Maximized(_)
            ))
    );
}

fn recording_job() -> (
    RecordingJob,
    RecordingControl,
    std::sync::mpsc::Sender<JobMessage>,
) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (controller, control) = RecordingController::channel();
    (
        RecordingJob {
            receiver,
            controller,
            cancellation: CancellationFlag::default(),
            paused: false,
            pause_requested: None,
            terminal_requested: false,
            retarget: None,
            snapshot_requests: std::collections::VecDeque::new(),
        },
        control,
        sender,
    )
}

#[test]
fn failed_or_discarded_recording_clears_live_progress_and_ignores_late_messages() {
    for completion in [
        RecordingCompletion::Failed {
            error: "window content dimensions changed".to_owned(),
            recovery_path: Some(PathBuf::from("/synthetic-qa/retained.gfsproj")),
        },
        RecordingCompletion::Discarded {
            cleanup_error: None,
        },
    ] {
        let mut app = GifFromScreenApp::default();
        let (job, _control, sender) = recording_job();
        app.job = Some(job);
        let progress = WorkflowProgress {
            phase: WorkflowPhase::Capturing,
            frames_captured: 209,
            capture_duration: Duration::from_secs(21),
            playback_duration: Duration::from_secs(21),
            encode: None,
        };
        sender.send(JobMessage::Progress(progress)).unwrap();
        sender.send(JobMessage::Finished(completion)).unwrap();
        sender.send(JobMessage::Progress(progress)).unwrap();
        sender.send(JobMessage::Persisting).unwrap();
        app.receive_job_messages();
        assert!(app.job.is_none());
        assert!(app.progress.is_none());
        assert!(app.restore_main_window);
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("Recoverable autosave retained") || notice.contains("discarded"));
        assert!(!notice.contains("Finalizing"));
    }
}

#[test]
fn wayland_controller_renders_only_the_root_surface_and_close_restores_the_shell() {
    // The renderer callback is thread-local. Isolate it so no other GUI test inherits it.
    std::thread::spawn(|| {
        egui::Context::set_immediate_viewport_renderer(|_, _| {
            panic!("Wayland controller must not create an immediate child surface")
        });
        let context = egui::Context::default();
        context.set_embed_viewports(false);
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ScreenRecorder;
        app.wayland_crop_controller = Some(controller(&context));
        let original_region = app.wayland_crop_controller.as_ref().unwrap().region;
        app.main_window_snapshot = Some(MainWindowSnapshot {
            position: None,
            size: egui::vec2(1040.0, 760.0),
            maximized: Some(true),
            restore_geometry: false,
        });
        let output = context.run(egui::RawInput::default(), |context| {
            app.show_wayland_crop_controller(context);
        });
        assert_eq!(output.viewport_output.len(), 1);
        assert_eq!(
            app.wayland_crop_controller.as_ref().unwrap().region,
            original_region
        );
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let output = context.run(input, |context| app.show_wayland_crop_controller(context));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        assert!(app.wayland_crop_controller.is_none());
        assert!(app.restore_main_window);
        let output = context.run(egui::RawInput::default(), |context| {
            app.restore_main_window_if_requested(context);
        });
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&egui::ViewportCommand::Title(APP_NAME.to_owned())));
        assert!(!commands.iter().any(|command| matches!(
            command,
            egui::ViewportCommand::Visible(_)
                | egui::ViewportCommand::OuterPosition(_)
                | egui::ViewportCommand::InnerSize(_)
                | egui::ViewportCommand::MinInnerSize(_)
                | egui::ViewportCommand::Maximized(_)
        )));
        assert!(!app.restore_main_window);
    })
    .join()
    .unwrap();
}

#[test]
fn active_controller_close_queues_stop_while_explicit_discard_remains_destructive() {
    for wayland in [true, false] {
        for (action, expected) in [
            (RecorderOverlayAction::Close, CaptureSessionState::Stopped),
            (
                RecorderOverlayAction::Discard,
                CaptureSessionState::Discarded,
            ),
        ] {
            let mut app = GifFromScreenApp::default();
            let (job, mut control, _sender) = recording_job();
            app.job = Some(job);
            let context = egui::Context::default();
            context.set_embed_viewports(false);
            let output = context.run(egui::RawInput::default(), |context| {
                if wayland {
                    app.handle_wayland_controller_action(context, action);
                } else {
                    context.show_viewport_deferred(
                        recorder_viewport_id(),
                        egui::ViewportBuilder::default(),
                        |_, _| {},
                    );
                    app.handle_recorder_overlay_action(context, action);
                }
            });
            if !wayland && action == RecorderOverlayAction::Close {
                assert!(
                    output.viewport_output[&recorder_viewport_id()]
                        .commands
                        .contains(&egui::ViewportCommand::CancelClose)
                );
            }
            assert_eq!(
                app.job.as_ref().unwrap().cancellation.is_cancelled(),
                action == RecorderOverlayAction::Discard
            );
            assert!(app.job.as_ref().unwrap().terminal_requested);
            let mut session = NoFramesSession::new();
            drain_control(&app, &mut session, &mut control);
            assert_eq!(
                session.events.first().copied(),
                Some(if expected == CaptureSessionState::Stopped {
                    "stop"
                } else {
                    "discard"
                })
            );
        }
    }
}

#[test]
fn controller_countdown_cancel_pause_ack_resume_and_stop_keep_existing_state_machine() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    app.wayland_crop_controller = Some(controller(&context));
    app.recording_countdown.start(Instant::now(), 3);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::CancelCountdown);
    assert!(!app.recording_countdown.is_active());
    assert!(app.wayland_crop_controller.is_some());
    let (job, mut control, _sender) = recording_job();
    app.job = Some(job);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Pause);
    app.job
        .as_mut()
        .unwrap()
        .acknowledge_phase(WorkflowPhase::Paused);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Resume);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Stop);
    let mut session = NoFramesSession::new();
    drain_control(&app, &mut session, &mut control);
    // The no-frame fixture is discarded during empty-recording cleanup, after Stop.
    assert_eq!(&session.events[..3], ["pause", "resume", "stop"]);
    assert!(!app.job.as_ref().unwrap().cancellation.is_cancelled());
}

#[test]
fn closing_an_active_controller_keeps_the_completed_project_and_returns_to_editor() {
    use gif_from_screen_application::{
        BlankAnimationProjectOptions, create_blank_animation_project,
    };
    use gif_from_screen_domain::{DurationUs, FrameId, ProjectId, Rgba, UnixTimeMs};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("close-saves.gfsproj");
    let project = create_blank_animation_project(
        &root,
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(1),
            frame_id: FrameId::from_u128(1),
            app_version: "controller-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            canvas: gif_from_screen_domain::PhysicalSize::new(1, 1).unwrap(),
            background: Rgba::TRANSPARENT,
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 4,
        },
    )
    .unwrap();
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    app.view = AppView::ScreenRecorder;
    app.wayland_crop_controller = Some(controller(&context));
    let (job, _control, sender) = recording_job();
    app.job = Some(job);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Close);
    assert!(!app.job.as_ref().unwrap().cancellation.is_cancelled());
    sender
        .send(JobMessage::Finished(RecordingCompletion::Completed(
            Box::new(project),
        )))
        .unwrap();
    app.receive_job_messages();
    assert_eq!(app.view, AppView::Editor);
    assert!(app.wayland_crop_controller.is_none());
    assert!(app.restore_main_window);
    let workspace = app.editor_workspace.as_ref().unwrap();
    assert_eq!(workspace.project_root(), root);
    assert_eq!(workspace.manifest().timeline.frames.len(), 1);
    assert!(root.join("manifest.json").is_file());
}

fn drain_control(
    app: &GifFromScreenApp,
    session: &mut NoFramesSession,
    control: &mut RecordingControl,
) {
    let _ = gif_from_screen_workflow::collect_prestarted_controlled_to_sink(
        session,
        &collection_options(&app.settings).unwrap(),
        control,
        &mut NoFramesSink,
        &gif_from_screen_gif::NeverCancel,
        &mut gif_from_screen_workflow::NoopWorkflowProgress,
    );
}

struct NoFramesSession {
    request: CaptureRequest,
    state: CaptureSessionState,
    events: Vec<&'static str>,
}
impl NoFramesSession {
    fn new() -> Self {
        Self {
            request: CaptureRequest::new(
                CaptureTarget::Monitor(CaptureSourceId::new("wayland-test").unwrap()),
                CaptureCadence::fixed_fps(10).unwrap(),
            ),
            state: CaptureSessionState::Recording,
            events: Vec::new(),
        }
    }
}
impl CaptureSession for NoFramesSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }
    fn request(&self) -> &CaptureRequest {
        &self.request
    }
    fn update_target(
        &mut self,
        target: CaptureTarget,
    ) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.request.target = target;
        Ok(())
    }
    fn pause(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Paused;
        self.events.push("pause");
        Ok(())
    }
    fn resume(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Recording;
        self.events.push("resume");
        Ok(())
    }
    fn stop(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Stopped;
        self.events.push("stop");
        Ok(())
    }
    fn discard(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Discarded;
        self.events.push("discard");
        Ok(())
    }
    fn poll_frame(
        &mut self,
        _: Duration,
    ) -> Result<FramePoll, gif_from_screen_capture::CaptureError> {
        panic!("terminal control must be applied before polling");
    }
}

struct NoFramesSink;
impl RecordingFrameSink for NoFramesSink {
    fn append_provisional_frame(
        &mut self,
        _: u64,
        _: &gif_from_screen_gif::RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame expected");
    }
    fn update_frame_duration(&mut self, _: u64, _: u64) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame expected");
    }
}
