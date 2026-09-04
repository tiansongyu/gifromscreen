#![forbid(unsafe_code)]

//! Desktop entry point for the Linux-first `GifFromScreen` application.

mod countdown;
mod editor_preview;
mod editor_ui;
mod editor_workspace;
mod export_job;
mod import_gif_job;
mod open_project_job;
mod retarget;

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use countdown::{CountdownStart, CountdownTick, MAX_COUNTDOWN_SECONDS, RecordingCountdown};
use editor_preview::EditorPreviewCache;
use editor_ui::{EditorUiState, show_editor_ui};
use editor_workspace::EditorWorkspace;
use eframe::egui;
use export_job::{ExportJob, ExportJobError, ExportJobEvent, ExportJobState};
use gif_from_screen_application::{
    ProjectExportSnapshot, ProjectFrameSelection, ProjectGifExportOptions, ProjectGifExportReport,
    RecordingProjectOptions, persist_collected_recording,
};
use gif_from_screen_capture::{
    CaptureBackend, CaptureCadence, CaptureRequest, CaptureSource, CaptureSourceId,
    CaptureSourceKind, CaptureTarget, CapturedFrame, CursorCaptureMode, PhysicalRect, PixelFormat,
};
use gif_from_screen_capture_linux::X11CaptureBackend;
use gif_from_screen_domain::{FrameId, ProjectId, UnixTimeMs};
use gif_from_screen_gif::{
    CancellationFlag, CancellationToken as _, DeltaMode, DitherMode, EncodeOptions, LoopBehavior,
    PaletteMode, QuantizerStrategy, Transparency,
};
use gif_from_screen_project::{ActiveProject, LockPolicy, OpenedProject};
use gif_from_screen_workflow::{
    CollectOptions, CollectedRecording, CollectionLimit, FrameRetention, RecordingControl,
    RecordingController, TargetUpdateRequest, TargetUpdateStatus, WorkflowProgress,
    collect_controlled,
};
use import_gif_job::{ImportGifJob, ImportGifJobEvent, ImportGifJobState};
use open_project_job::{OpenProjectJob, OpenProjectJobEvent, OpenProjectJobState};
use retarget::{RegionRetargetPlan, RetargetCompletion};
use uuid::Uuid;

const APP_NAME: &str = "GifFromScreen";
const RECORDER_BORDER_POINTS: f32 = 4.0;
const RECORDER_TOOLBAR_POINTS: f32 = 76.0;
const MAX_RECORDING_DURATION_MS: u64 = 3_600_000;
const EDITOR_HISTORY_LIMIT: usize = 100;
const EDITOR_PREVIEW_MAX_SIZE: [u32; 2] = [960, 540];
const LANDING_COLUMN_COUNT: usize = 2;
const LANDING_CARD_MIN_WIDTH: f32 = 280.0;

fn recorder_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("gif-from-screen-recorder-frame")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AppView {
    #[default]
    Landing,
    OpenProject,
    ImportGif,
    ScreenRecorder,
    Editor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StartupIntent {
    None,
    OpenProject(PathBuf),
    ImportGif(PathBuf),
    Invalid(String),
}

#[derive(Clone, Debug)]
struct RecordingSettings {
    output: String,
    duration_ms: u64,
    fps: u32,
    countdown_seconds: u8,
    changes_only: bool,
    region_enabled: bool,
    region_x: i32,
    region_y: i32,
    region_width: u32,
    region_height: u32,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        let output = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("gif-from-screen.gif")
            .to_string_lossy()
            .into_owned();
        Self {
            output,
            duration_ms: 0,
            fps: 10,
            countdown_seconds: 3,
            changes_only: false,
            region_enabled: true,
            region_x: 0,
            region_y: 0,
            region_width: 640,
            region_height: 480,
        }
    }
}

enum JobMessage {
    Progress(WorkflowProgress),
    Persisting,
    Finished(Result<Box<ActiveProject>, String>),
}

struct RecordingWorkerRequest {
    settings: RecordingSettings,
    source_id: CaptureSourceId,
    source_kind: CaptureSourceKind,
    source_label: String,
    project_path: PathBuf,
}

struct CompletedProjectSummary {
    frames: usize,
    duration_us: u64,
    project_path: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportFrameScope {
    #[default]
    All,
    Selected,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportPaletteChoice {
    #[default]
    Local,
    Global,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportQuantizerChoice {
    #[default]
    MedianCut,
    Octree,
    Grayscale,
    MostUsed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportDitherChoice {
    #[default]
    None,
    Bayer,
    FloydSteinberg,
    Sierra,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportLoopChoice {
    #[default]
    Infinite,
    Finite,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditorExportSettings {
    frame_scope: ExportFrameScope,
    max_colors: u16,
    palette: ExportPaletteChoice,
    quantizer: ExportQuantizerChoice,
    dither: ExportDitherChoice,
    delta: bool,
    alpha_threshold: u8,
    loop_choice: ExportLoopChoice,
    finite_loop_count: u16,
    overwrite: bool,
}

impl Default for EditorExportSettings {
    fn default() -> Self {
        Self {
            frame_scope: ExportFrameScope::All,
            max_colors: 256,
            palette: ExportPaletteChoice::Local,
            quantizer: ExportQuantizerChoice::MedianCut,
            dither: ExportDitherChoice::None,
            delta: false,
            alpha_threshold: 1,
            loop_choice: ExportLoopChoice::Infinite,
            finite_loop_count: 1,
            overwrite: false,
        }
    }
}

struct RecordingJob {
    receiver: Receiver<JobMessage>,
    cancellation: CancellationFlag,
    controller: RecordingController,
    paused: bool,
    terminal_requested: bool,
    retarget: Option<RecordingRetarget>,
}

struct RecordingRetarget {
    source: CaptureSourceId,
    plan: RegionRetargetPlan,
    pending: Option<TargetUpdateRequest>,
}

impl RecordingRetarget {
    fn new(source: CaptureSourceId, initial: PhysicalRect) -> Self {
        Self {
            source,
            plan: RegionRetargetPlan::new(initial),
            pending: None,
        }
    }

    fn observe(&mut self, controller: &RecordingController, candidate: PhysicalRect) {
        if let Some(region) = self.plan.observe(candidate) {
            self.send(controller, region);
        }
    }

    fn poll(&mut self, controller: &RecordingController, allow_next: bool) -> Option<String> {
        let status = self.pending.as_mut()?.status();
        let (completion, notice) = match status {
            TargetUpdateStatus::Applied => (RetargetCompletion::Applied, None),
            TargetUpdateStatus::Rejected(error) => (
                RetargetCompletion::Rejected,
                Some(format!(
                    "Could not move the capture area; recording continues at its last accepted position: {error}"
                )),
            ),
            TargetUpdateStatus::WorkerExited => (
                RetargetCompletion::WorkerExited,
                Some(
                    "Could not move the capture area because the recording worker has exited."
                        .to_owned(),
                ),
            ),
            // Pending and future non-terminal states remain in flight.
            _ => return None,
        };
        self.pending = None;
        if let Some(region) = self.plan.complete(completion, allow_next) {
            self.send(controller, region);
        }
        notice
    }

    fn disable(&mut self) {
        self.plan.disable();
    }

    fn send(&mut self, controller: &RecordingController, region: PhysicalRect) {
        debug_assert!(self.pending.is_none());
        self.pending = Some(controller.update_target(CaptureTarget::Region {
            source: self.source.clone(),
            region,
        }));
    }
}

impl RecordingJob {
    fn observe_target(&mut self, candidate: PhysicalRect) {
        if self.terminal_requested {
            return;
        }
        if let Some(retarget) = &mut self.retarget {
            retarget.observe(&self.controller, candidate);
        }
    }

    fn poll_retarget(&mut self, allow_next: bool) -> Option<String> {
        self.retarget
            .as_mut()?
            .poll(&self.controller, allow_next && !self.terminal_requested)
    }

    fn stop_retargeting(&mut self) {
        self.terminal_requested = true;
        if let Some(retarget) = &mut self.retarget {
            retarget.disable();
        }
    }
}

struct RegionPicker {
    texture: egui::TextureHandle,
    source_width: u32,
    source_height: u32,
    drag_start: Option<egui::Pos2>,
    drag_current: Option<egui::Pos2>,
    selection: Option<PhysicalRect>,
}

struct RecorderOverlay {
    initial_position: egui::Pos2,
    initial_size: egui::Vec2,
    initialized: bool,
    source_geometry: PhysicalRect,
}

#[derive(Clone, Copy, Debug)]
struct MainWindowSnapshot {
    position: egui::Pos2,
    size: egui::Vec2,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RecorderOverlayAction {
    #[default]
    None,
    Start,
    CancelCountdown,
    Pause,
    Resume,
    Stop,
    Discard,
    Close,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RecorderStage {
    #[default]
    Ready,
    Countdown(u8),
    Recording,
    Paused,
    Finalizing,
}

impl RecorderStage {
    const fn allows_moving(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Countdown(_) | Self::Recording | Self::Paused
        )
    }

    const fn allows_resizing(self) -> bool {
        matches!(self, Self::Ready)
    }

    const fn allows_retargeting(self) -> bool {
        matches!(self, Self::Recording | Self::Paused)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RecorderOverlayFrame {
    action: RecorderOverlayAction,
    region: Option<PhysicalRect>,
}

struct GifFromScreenApp {
    view: AppView,
    notice: Option<String>,
    settings: RecordingSettings,
    sources: Vec<CaptureSource>,
    selected_source: usize,
    region_picker: Option<RegionPicker>,
    recorder_overlay: Option<RecorderOverlay>,
    main_window_snapshot: Option<MainWindowSnapshot>,
    restore_main_window: bool,
    recording_countdown: RecordingCountdown,
    job: Option<RecordingJob>,
    progress: Option<WorkflowProgress>,
    open_project_path: String,
    open_project_job: OpenProjectJob,
    import_gif_path: String,
    import_gif_job: ImportGifJob,
    editor_workspace: Option<EditorWorkspace>,
    editor_ui_state: EditorUiState,
    editor_preview_cache: EditorPreviewCache,
    editor_export_settings: EditorExportSettings,
    export_job: ExportJob,
}

impl Default for GifFromScreenApp {
    fn default() -> Self {
        let (sources, notice) = match load_x11_sources() {
            Ok(sources) => (sources, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let selected_source = sources
            .iter()
            .position(|source| !source.name().contains("(root)"))
            .unwrap_or(0);
        Self {
            view: AppView::Landing,
            notice,
            settings: RecordingSettings::default(),
            sources,
            selected_source,
            region_picker: None,
            recorder_overlay: None,
            main_window_snapshot: None,
            restore_main_window: false,
            recording_countdown: RecordingCountdown::default(),
            job: None,
            progress: None,
            open_project_path: String::new(),
            open_project_job: OpenProjectJob::default(),
            import_gif_path: String::new(),
            import_gif_job: ImportGifJob::default(),
            editor_workspace: None,
            editor_ui_state: EditorUiState::default(),
            editor_preview_cache: EditorPreviewCache::new(),
            editor_export_settings: EditorExportSettings::default(),
            export_job: ExportJob::default(),
        }
    }
}

impl Drop for GifFromScreenApp {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            let _ = job.controller.discard();
            job.cancellation.cancel();
        }
    }
}

impl eframe::App for GifFromScreenApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_job_messages();
        self.receive_export_messages();
        self.receive_open_project_messages();
        self.receive_import_gif_messages();
        self.advance_recording_countdown(context);
        if self.restore_main_window {
            if let Some(snapshot) = self.main_window_snapshot.take() {
                context.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                context.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                    680.0, 440.0,
                )));
                context.send_viewport_cmd(egui::ViewportCommand::InnerSize(snapshot.size));
                context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(snapshot.position));
            }
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.restore_main_window = false;
        }
        if self.recorder_overlay.is_some() {
            self.show_recorder_overlay(context);
        }
        if self.job.is_some()
            || self.recorder_overlay.is_some()
            || self.recording_countdown.is_active()
            || export_job_is_active(self.export_job.state())
            || self.open_project_job.state() == OpenProjectJobState::Running
            || self.import_gif_job.state() == ImportGifJobState::Running
        {
            context.request_repaint_after(Duration::from_millis(33));
        }

        egui::TopBottomPanel::top("app_header").show(context, |ui| {
            ui.horizontal(|ui| {
                let back_enabled = can_navigate_back(
                    self.view,
                    self.open_project_job.state(),
                    self.import_gif_job.state(),
                );
                if self.view != AppView::Landing
                    && ui
                        .add_enabled(back_enabled, egui::Button::new("Back"))
                        .clicked()
                {
                    self.view = AppView::Landing;
                }
                ui.heading(APP_NAME);
                ui.separator();
                ui.label("Linux X11 preview");
            });
        });

        egui::CentralPanel::default().show(context, |ui| match self.view {
            AppView::Landing => self.show_landing(ui),
            AppView::OpenProject => self.show_open_project(ui),
            AppView::ImportGif => self.show_import_gif(ui),
            AppView::ScreenRecorder => self.show_screen_recorder(ui),
            AppView::Editor => self.show_editor(ui),
        });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

impl GifFromScreenApp {
    fn apply_startup_intent(&mut self, intent: StartupIntent) {
        match intent {
            StartupIntent::None => {}
            StartupIntent::OpenProject(path) => {
                self.view = AppView::OpenProject;
                self.open_project_path = path.to_string_lossy().into_owned();
                if let Err(error) = self.start_open_project() {
                    self.notice = Some(format!("Could not open startup project: {error}"));
                }
            }
            StartupIntent::ImportGif(path) => {
                self.view = AppView::ImportGif;
                self.import_gif_path = path.to_string_lossy().into_owned();
                if let Err(error) = self.start_import_gif() {
                    self.notice = Some(format!("Could not import startup GIF: {error}"));
                }
            }
            StartupIntent::Invalid(message) => self.notice = Some(message),
        }
    }

    fn show_landing(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.heading("Create an animated GIF");
            ui.label("Capture, edit frame by frame, and export locally.");
            ui.add_space(28.0);

            ui.columns(LANDING_COLUMN_COUNT, |columns| {
                if landing_action(
                    &mut columns[0],
                    "Screen recorder",
                    "Record an X11 monitor or physical-pixel region.",
                    true,
                ) {
                    self.view = AppView::ScreenRecorder;
                }
                if landing_action(
                    &mut columns[1],
                    "Open project",
                    "Open an existing editable .gfsproj directory.",
                    true,
                ) {
                    self.view = AppView::OpenProject;
                    self.notice = None;
                }
            });

            ui.add_space(12.0);
            ui.columns(LANDING_COLUMN_COUNT, |columns| {
                if landing_action(
                    &mut columns[0],
                    "Import GIF",
                    "Decode a GIF safely into a new editable project.",
                    true,
                ) {
                    self.view = AppView::ImportGif;
                    self.notice = Some(
                        "GIF import uses strict 10,000-frame, 16K-canvas, and 512 MiB limits. Import cannot currently be cancelled once started."
                            .to_owned(),
                    );
                }
                let _ = landing_action(
                    &mut columns[1],
                    "Webcam recorder",
                    "Create an animated GIF from a camera.",
                    false,
                );
            });

            ui.add_space(12.0);
            ui.columns(LANDING_COLUMN_COUNT, |columns| {
                let _ = landing_action(
                    &mut columns[0],
                    "Drawing board",
                    "Record drawing strokes as an animation.",
                    false,
                );
            });

            if let Some(notice) = &self.notice {
                ui.add_space(24.0);
                ui.label(notice);
            }
        });
    }

    fn show_open_project(&mut self, ui: &mut egui::Ui) {
        let running = self.open_project_job.state() == OpenProjectJobState::Running;
        ui.heading("Open editable project");
        ui.label("Choose an existing .gfsproj directory containing manifest.json.");
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("Project directory");
            ui.add_enabled(
                !running,
                egui::TextEdit::singleline(&mut self.open_project_path)
                    .desired_width(420.0)
                    .hint_text("/path/to/animation.gfsproj"),
            );
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Open"))
                .clicked()
                && let Err(error) = self.start_open_project()
            {
                self.notice = Some(format!("Could not start opening project: {error}"));
            }
            if ui
                .add_enabled(
                    can_navigate_back(
                        self.view,
                        self.open_project_job.state(),
                        self.import_gif_job.state(),
                    ),
                    egui::Button::new("Back"),
                )
                .clicked()
            {
                self.view = AppView::Landing;
            }
            if running {
                ui.spinner();
                ui.label("Opening and recovering project…");
            }
        });
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn start_open_project(&mut self) -> Result<(), String> {
        let path = self.open_project_path.trim();
        if path.is_empty() {
            return Err("Select a .gfsproj directory first.".to_owned());
        }
        self.open_project_job
            .start(PathBuf::from(path), LockPolicy::FailIfPresent)
            .map_err(|error| error.to_string())?;
        self.notice = Some("Opening project in the background…".to_owned());
        Ok(())
    }

    fn show_import_gif(&mut self, ui: &mut egui::Ui) {
        let running = self.import_gif_job.state() == ImportGifJobState::Running;
        ui.heading("Import GIF as editable project");
        ui.label(
            "Choose a regular .gif file. The project will be created beside it as <stem>.gfsproj.",
        );
        ui.weak(
            "Safety limits: 10,000 frames, 16K canvas dimensions, and 512 MiB decoded RGBA. This operation cannot currently be cancelled once started.",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("GIF file");
            ui.add_enabled(
                !running,
                egui::TextEdit::singleline(&mut self.import_gif_path)
                    .desired_width(420.0)
                    .hint_text("/path/to/animation.gif"),
            );
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Import"))
                .clicked()
                && let Err(error) = self.start_import_gif()
            {
                self.notice = Some(format!("Could not start GIF import: {error}"));
            }
            if ui
                .add_enabled(
                    can_navigate_back(
                        self.view,
                        self.open_project_job.state(),
                        self.import_gif_job.state(),
                    ),
                    egui::Button::new("Back"),
                )
                .clicked()
            {
                self.view = AppView::Landing;
            }
            if running {
                ui.spinner();
                ui.label("Decoding and creating project… this operation is not cancellable.");
            }
        });
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn start_import_gif(&mut self) -> Result<(), String> {
        let path = self.import_gif_path.trim();
        if path.is_empty() {
            return Err("Select a .gif file first.".to_owned());
        }
        self.import_gif_job
            .start(PathBuf::from(path))
            .map_err(|error| error.to_string())?;
        self.notice = Some(
            "Importing GIF in the background. The bounded decode/persist operation cannot be cancelled."
                .to_owned(),
        );
        Ok(())
    }

    fn show_screen_recorder(&mut self, ui: &mut egui::Ui) {
        if self.region_picker.is_some() {
            self.show_region_picker(ui);
            return;
        }
        ui.heading("X11 screen recorder");
        ui.label("Capture and durable project creation run on a background worker.");
        ui.add_space(12.0);
        self.show_recording_settings(ui);

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.job.is_none() && self.recorder_overlay.is_none(),
                    egui::Button::new("Open recorder frame"),
                )
                .clicked()
                && let Err(error) = self.open_recorder_overlay(ui.ctx())
            {
                self.notice = Some(error);
            }
        });

        if let Some(progress) = self.progress {
            ui.add_space(12.0);
            ui.label(format!(
                "{:?}: {} captured frames, {:.2}s",
                progress.phase,
                progress.frames_captured,
                progress.capture_duration.as_secs_f32()
            ));
        }
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        show_editor_scroll_area(ui, |ui| self.show_editor_contents(ui));
    }

    fn show_editor_contents(&mut self, ui: &mut egui::Ui) {
        let Some(workspace) = &mut self.editor_workspace else {
            ui.label("No active editor project.");
            return;
        };
        let results = show_editor_ui(ui, workspace, &mut self.editor_ui_state);
        for failure in results.into_iter().filter_map(Result::err) {
            self.notice = Some(format!(
                "Editor {:?} failed: {}",
                failure.operation, failure.message
            ));
        }

        ui.separator();
        show_editor_preview_panel(ui, workspace, &mut self.editor_preview_cache);
        ui.separator();
        let selected_count = workspace.selection().len();
        let asset_issue_count = workspace.asset_issues().len();
        let export_action = show_export_panel(
            ui,
            &mut self.settings.output,
            &mut self.editor_export_settings,
            &self.export_job,
            selected_count,
            asset_issue_count,
        );
        match export_action {
            EditorExportAction::None => {}
            EditorExportAction::Start => {
                if let Err(error) = self.start_editor_export() {
                    self.notice = Some(error);
                }
            }
            EditorExportAction::Cancel => {
                if self.export_job.cancel() {
                    self.notice = Some("Cancelling GIF export…".to_owned());
                }
            }
        }
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn show_recording_settings(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("recording_settings")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Capture source");
                ui.horizontal(|ui| {
                    let selected_name = self
                        .sources
                        .get(self.selected_source)
                        .map_or_else(|| "No X11 source".to_owned(), |source| source.name().into());
                    egui::ComboBox::from_id_salt("capture_source")
                        .selected_text(selected_name)
                        .show_ui(ui, |ui| {
                            for (index, source) in self.sources.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected_source,
                                    index,
                                    source.name(),
                                );
                            }
                        });
                    if ui.button("Refresh").clicked() {
                        self.refresh_sources();
                    }
                });
                ui.end_row();

                ui.label("Output GIF");
                ui.text_edit_singleline(&mut self.settings.output);
                ui.end_row();
                ui.label("Maximum duration (ms, 0 = manual stop)");
                ui.add(
                    egui::DragValue::new(&mut self.settings.duration_ms)
                        .range(0..=MAX_RECORDING_DURATION_MS),
                );
                ui.end_row();
                ui.label("Frames per second");
                ui.add(egui::DragValue::new(&mut self.settings.fps).range(1..=60));
                ui.end_row();
                ui.label("Frame retention");
                ui.checkbox(
                    &mut self.settings.changes_only,
                    "Store only frames whose pixels changed",
                );
                ui.end_row();
                ui.label("Start countdown (seconds)");
                ui.add(
                    egui::DragValue::new(&mut self.settings.countdown_seconds)
                        .range(0..=MAX_COUNTDOWN_SECONDS),
                );
                ui.end_row();
                ui.label("Capture a region");
                ui.checkbox(
                    &mut self.settings.region_enabled,
                    "Use physical-pixel rectangle",
                );
                ui.end_row();
            });

        if self.settings.region_enabled {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("X");
                ui.add(egui::DragValue::new(&mut self.settings.region_x));
                ui.label("Y");
                ui.add(egui::DragValue::new(&mut self.settings.region_y));
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut self.settings.region_width).range(1..=65_535));
                ui.label("Height");
                ui.add(egui::DragValue::new(&mut self.settings.region_height).range(1..=65_535));
            });
            if ui.button("Select region visually").clicked()
                && let Err(error) = self.begin_region_picker(ui.ctx())
            {
                self.notice = Some(error);
            }
        }
        if let Some(source) = self.sources.get(self.selected_source)
            && let Some(rect) = source.geometry()
        {
            ui.weak(format!(
                "Selected source: {}×{} at {},{} ({:?})",
                rect.size().width(),
                rect.size().height(),
                rect.origin().x,
                rect.origin().y,
                source.kind()
            ));
        }
    }

    fn begin_region_picker(&mut self, context: &egui::Context) -> Result<(), String> {
        let source = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No X11 capture source is selected.".to_owned())?;
        let target = match source.kind() {
            CaptureSourceKind::Monitor => CaptureTarget::Monitor(source.id().clone()),
            CaptureSourceKind::Window => CaptureTarget::Window(source.id().clone()),
            _ => return Err("Unsupported future X11 capture source kind.".into()),
        };
        let backend = X11CaptureBackend::connect(None)
            .map_err(|error| format!("Could not connect to X11: {error}"))?;
        let frame = backend
            .capture_once(&target)
            .map_err(|error| format!("Could not capture region preview: {error}"))?;
        let (image, source_width, source_height) = frame_to_preview(&frame)?;
        let texture = context.load_texture(
            format!("region-preview-{}", source.id()),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.region_picker = Some(RegionPicker {
            texture,
            source_width,
            source_height,
            drag_start: None,
            drag_current: None,
            selection: None,
        });
        self.notice = None;
        Ok(())
    }

    // egui geometry is f32 while capture dimensions are exact u32 values. The
    // picker converts back against the source dimensions before committing.
    #[allow(clippy::cast_precision_loss)]
    fn show_region_picker(&mut self, ui: &mut egui::Ui) {
        let mut apply = None;
        let mut cancel = false;
        let picker = self
            .region_picker
            .as_mut()
            .expect("caller checked region picker presence");

        ui.heading("Select capture region");
        ui.label("Drag over the preview, then apply the physical-pixel rectangle.");
        ui.add_space(8.0);
        let available = ui.available_size();
        let maximum = egui::vec2(available.x.max(1.0), (available.y - 90.0).max(1.0));
        let scale = (maximum.x / picker.source_width as f32)
            .min(maximum.y / picker.source_height as f32)
            .min(1.0);
        let image_size = egui::vec2(
            picker.source_width as f32 * scale,
            picker.source_height as f32 * scale,
        );
        let response = ui.add(
            egui::Image::new(&picker.texture)
                .fit_to_exact_size(image_size)
                .sense(egui::Sense::drag()),
        );
        if response.drag_started() {
            picker.drag_start = response.interact_pointer_pos();
            picker.drag_current = picker.drag_start;
            picker.selection = None;
        }
        if response.dragged() {
            picker.drag_current = response.interact_pointer_pos();
        }
        if let (Some(start), Some(current)) = (picker.drag_start, picker.drag_current) {
            let selection = egui::Rect::from_two_pos(
                clamp_to_rect(start, response.rect),
                clamp_to_rect(current, response.rect),
            );
            ui.painter().rect_stroke(
                selection,
                0.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(242, 153, 74)),
                egui::StrokeKind::Inside,
            );
            if response.drag_stopped() {
                picker.selection = map_preview_selection(
                    response.rect,
                    selection,
                    picker.source_width,
                    picker.source_height,
                );
            }
        }

        ui.horizontal(|ui| {
            if let Some(selection) = picker.selection {
                ui.label(format!(
                    "{}×{} at {},{}",
                    selection.size().width(),
                    selection.size().height(),
                    selection.origin().x,
                    selection.origin().y
                ));
            } else {
                ui.weak("No region selected");
            }
            if ui
                .add_enabled(picker.selection.is_some(), egui::Button::new("Apply"))
                .clicked()
            {
                apply = picker.selection;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });

        if let Some(selection) = apply {
            self.settings.region_enabled = true;
            self.settings.region_x = selection.origin().x;
            self.settings.region_y = selection.origin().y;
            self.settings.region_width = selection.size().width();
            self.settings.region_height = selection.size().height();
            self.notice = Some("Capture region updated from preview.".into());
            self.region_picker = None;
        } else if cancel {
            self.region_picker = None;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn open_recorder_overlay(&mut self, context: &egui::Context) -> Result<(), String> {
        validate_settings(&self.settings)?;
        let main_window = context
            .input(|input| {
                let viewport = input.viewport();
                Some(MainWindowSnapshot {
                    position: viewport.outer_rect?.min,
                    size: viewport.inner_rect?.size(),
                })
            })
            .ok_or_else(|| "Could not read the main window geometry.".to_owned())?;
        let source = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No X11 capture source is selected.".to_owned())?;
        let source_geometry = source
            .geometry()
            .ok_or_else(|| "The selected source has no usable geometry.".to_owned())?;
        let region = if self.settings.region_enabled {
            PhysicalRect::new(
                self.settings.region_x,
                self.settings.region_y,
                self.settings.region_width,
                self.settings.region_height,
            )
            .map_err(|error| error.to_string())?
        } else {
            PhysicalRect::new(
                0,
                0,
                source_geometry.size().width(),
                source_geometry.size().height(),
            )
            .map_err(|error| error.to_string())?
        };
        if !region.fits_within(source_geometry.size()) {
            return Err("The capture rectangle must stay inside the selected source.".into());
        }

        let pixels_per_point = context.input(|input| {
            input
                .viewport()
                .native_pixels_per_point
                .unwrap_or_else(|| context.pixels_per_point())
        });
        let absolute_x = source_geometry
            .origin()
            .x
            .checked_add(region.origin().x)
            .ok_or_else(|| "Recorder X position overflowed.".to_owned())?;
        let absolute_y = source_geometry
            .origin()
            .y
            .checked_add(region.origin().y)
            .ok_or_else(|| "Recorder Y position overflowed.".to_owned())?;
        let position = egui::pos2(
            absolute_x as f32 / pixels_per_point - RECORDER_BORDER_POINTS,
            absolute_y as f32 / pixels_per_point - RECORDER_BORDER_POINTS,
        );
        let size = egui::vec2(
            region.size().width() as f32 / pixels_per_point + RECORDER_BORDER_POINTS * 2.0,
            region.size().height() as f32 / pixels_per_point
                + RECORDER_BORDER_POINTS * 2.0
                + RECORDER_TOOLBAR_POINTS,
        );
        self.recorder_overlay = Some(RecorderOverlay {
            initial_position: position,
            initial_size: size,
            initialized: false,
            source_geometry,
        });
        self.main_window_snapshot = Some(main_window);
        self.notice = Some("Recorder frame opened. Move or resize it, then press Start.".into());
        context.request_repaint();
        context.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(1.0, 1.0)));
        context.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        context.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1.0, 1.0)));
        context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            -10_000.0, -10_000.0,
        )));
        Ok(())
    }

    fn show_recorder_overlay(&mut self, context: &egui::Context) {
        let Some(overlay) = &self.recorder_overlay else {
            return;
        };
        let stage = self.recorder_stage();
        if stage == RecorderStage::Finalizing
            && let Some(job) = &mut self.job
        {
            job.stop_retargeting();
        }
        let retarget_notice = self
            .job
            .as_mut()
            .and_then(|job| job.poll_retarget(stage.allows_retargeting()));
        if retarget_notice.is_some() {
            self.notice = retarget_notice;
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title("GifFromScreen recorder")
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(stage.allows_resizing())
            .with_movable_by_background(stage.allows_moving())
            .with_min_inner_size([180.0, 130.0])
            .with_always_on_top()
            .with_has_shadow(false)
            .with_taskbar(false)
            .with_window_type(egui::X11WindowType::Utility);
        if !overlay.initialized {
            builder = builder
                .with_position(overlay.initial_position)
                .with_inner_size(overlay.initial_size);
        }
        let progress = self.progress;
        let source_geometry = overlay.source_geometry;
        let frame = context.show_viewport_immediate(
            recorder_viewport_id(),
            builder,
            |viewport_context, _class| {
                draw_recorder_overlay(viewport_context, stage, progress, source_geometry)
            },
        );
        if let Some(overlay) = &mut self.recorder_overlay {
            overlay.initialized = true;
        }
        if let Some(region) = frame.region {
            apply_overlay_region(&mut self.settings, stage, region);
            if should_sync_retarget(stage, frame.action)
                && let Some(job) = &mut self.job
            {
                job.observe_target(region);
            }
        }
        self.handle_recorder_overlay_action(context, frame.action);
    }

    fn recorder_stage(&self) -> RecorderStage {
        if let Some(remaining) = self.recording_countdown.remaining_seconds() {
            return RecorderStage::Countdown(remaining);
        }
        let Some(job) = &self.job else {
            return RecorderStage::Ready;
        };
        if job.terminal_requested {
            return RecorderStage::Finalizing;
        }
        if job.paused {
            return RecorderStage::Paused;
        }
        match self.progress.map(|progress| progress.phase) {
            Some(
                gif_from_screen_workflow::WorkflowPhase::Encoding
                | gif_from_screen_workflow::WorkflowPhase::Committing,
            ) => RecorderStage::Finalizing,
            _ => RecorderStage::Recording,
        }
    }

    fn handle_recorder_overlay_action(
        &mut self,
        context: &egui::Context,
        action: RecorderOverlayAction,
    ) {
        match action {
            RecorderOverlayAction::None => {}
            RecorderOverlayAction::Start => {
                if let Err(error) = self.begin_recording(context) {
                    self.notice = Some(error);
                }
            }
            RecorderOverlayAction::CancelCountdown => {
                if self.recording_countdown.cancel() {
                    self.notice = Some("Recording countdown cancelled.".into());
                    context.request_repaint();
                }
            }
            RecorderOverlayAction::Pause => {
                if let Some(job) = &mut self.job
                    && job.controller.pause()
                {
                    job.paused = true;
                }
            }
            RecorderOverlayAction::Resume => {
                if let Some(job) = &mut self.job
                    && job.controller.resume()
                {
                    job.paused = false;
                }
            }
            RecorderOverlayAction::Stop => {
                if let Some(job) = &mut self.job {
                    job.stop_retargeting();
                    let _ = job.controller.stop();
                    self.notice = Some("Stopping and encoding…".into());
                }
            }
            RecorderOverlayAction::Discard | RecorderOverlayAction::Close => {
                if let Some(job) = &mut self.job {
                    job.stop_retargeting();
                    let _ = job.controller.discard();
                    job.cancellation.cancel();
                    self.notice = Some("Discarding recording…".into());
                } else {
                    self.close_recorder_overlay();
                }
            }
        }
    }

    fn close_recorder_overlay(&mut self) {
        self.recording_countdown.cancel();
        self.recorder_overlay = None;
        self.restore_main_window = true;
    }

    fn begin_recording(&mut self, context: &egui::Context) -> Result<(), String> {
        validate_settings(&self.settings)?;
        if self.job.is_some() {
            return Ok(());
        }
        match self
            .recording_countdown
            .start(Instant::now(), self.settings.countdown_seconds)
        {
            CountdownStart::Immediate => self.start_recording(),
            CountdownStart::Started => {
                self.notice = Some(format!(
                    "Recording starts in {} seconds…",
                    self.settings.countdown_seconds
                ));
                context.request_repaint();
                Ok(())
            }
            CountdownStart::AlreadyRunning => Ok(()),
            CountdownStart::OutOfRange => Err(format!(
                "Countdown must be between 0 and {MAX_COUNTDOWN_SECONDS} seconds."
            )),
        }
    }

    fn advance_recording_countdown(&mut self, context: &egui::Context) {
        match self.recording_countdown.tick(Instant::now()) {
            CountdownTick::Idle => {}
            CountdownTick::Waiting(_) => {
                context.request_repaint_after(Duration::from_millis(16));
            }
            CountdownTick::Finished => {
                context.request_repaint();
                if self.recorder_overlay.is_some()
                    && self.job.is_none()
                    && let Err(error) = self.start_recording()
                {
                    self.notice = Some(error);
                }
            }
        }
    }

    fn start_recording(&mut self) -> Result<(), String> {
        validate_settings(&self.settings)?;
        let selected = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No X11 capture source is selected.".to_owned())?;
        let settings = self.settings.clone();
        let source_id = selected.id().clone();
        let source_kind = selected.kind();
        let project_path = project_path_for_output(Path::new(settings.output.trim()))?;
        let retarget = if settings.region_enabled {
            let initial = PhysicalRect::new(
                settings.region_x,
                settings.region_y,
                settings.region_width,
                settings.region_height,
            )
            .map_err(|error| error.to_string())?;
            Some(RecordingRetarget::new(source_id.clone(), initial))
        } else {
            None
        };
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let (controller, mut control) = RecordingController::channel();
        let (sender, receiver) = mpsc::channel();
        let worker_request = RecordingWorkerRequest {
            settings,
            source_id,
            source_kind,
            source_label: selected.name().to_owned(),
            project_path,
        };

        std::thread::Builder::new()
            .name("gfs-x11-record".into())
            .spawn(move || {
                let progress_sender = sender.clone();
                let mut progress = move |snapshot| {
                    let _ = progress_sender.send(JobMessage::Progress(snapshot));
                };
                let result = collect_x11_recording(
                    &worker_request,
                    &mut control,
                    &worker_cancellation,
                    &mut progress,
                )
                .and_then(|recording| {
                    ensure_recording_not_cancelled(&worker_cancellation)?;
                    let _ = sender.send(JobMessage::Persisting);
                    persist_recording_project(&worker_request, recording, &worker_cancellation)
                })
                .map(Box::new)
                .map_err(|error| error.to_string());
                if let Err(error) = sender.send(JobMessage::Finished(result))
                    && worker_cancellation.is_cancelled()
                    && let JobMessage::Finished(Ok(project)) = error.0
                {
                    let _ = remove_completed_project(*project);
                }
            })
            .map_err(|error| format!("could not start recording worker: {error}"))?;

        self.notice = Some("Recording started…".into());
        self.progress = None;
        self.job = Some(RecordingJob {
            receiver,
            cancellation,
            controller,
            paused: false,
            terminal_requested: false,
            retarget,
        });
        Ok(())
    }

    fn refresh_sources(&mut self) {
        match load_x11_sources() {
            Ok(sources) => {
                self.sources = sources;
                self.selected_source = self
                    .selected_source
                    .min(self.sources.len().saturating_sub(1));
                self.notice = Some(format!("Found {} X11 capture sources.", self.sources.len()));
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn receive_job_messages(&mut self) {
        let Some(job) = &self.job else {
            return;
        };
        let messages: Vec<_> = job.receiver.try_iter().collect();
        for message in messages {
            match message {
                JobMessage::Progress(progress) => self.progress = Some(progress),
                JobMessage::Persisting => {
                    if let Some(job) = &mut self.job {
                        job.stop_retargeting();
                    }
                    self.notice = Some("Saving editable project…".to_owned());
                }
                JobMessage::Finished(Ok(project)) => {
                    let discarded = self
                        .job
                        .as_ref()
                        .is_some_and(|job| job.cancellation.is_cancelled());
                    if discarded {
                        self.notice = Some(match remove_completed_project(*project) {
                            Ok(()) => "Recording discarded.".to_owned(),
                            Err(error) => format!("Recording was discarded, but {error}"),
                        });
                    } else {
                        self.notice = Some(
                            match activate_editor(
                                &mut self.view,
                                &mut self.editor_workspace,
                                *project,
                            ) {
                                Ok(summary) => {
                                    self.editor_ui_state = EditorUiState::default();
                                    self.editor_preview_cache = EditorPreviewCache::new();
                                    self.editor_export_settings = EditorExportSettings::default();
                                    format!(
                                        "Project ready: {} frames, {:.3}s at {}",
                                        summary.frames,
                                        Duration::from_micros(summary.duration_us).as_secs_f64(),
                                        summary.project_path.display()
                                    )
                                }
                                Err(error) => format!("Could not open recorded project: {error}"),
                            },
                        );
                    }
                    self.finish_recording_job();
                }
                JobMessage::Finished(Err(error)) => {
                    let discarded = self
                        .job
                        .as_ref()
                        .is_some_and(|job| job.cancellation.is_cancelled());
                    self.notice = Some(if discarded {
                        "Recording discarded.".to_owned()
                    } else {
                        format!("Recording failed: {error}")
                    });
                    self.finish_recording_job();
                }
            }
        }
    }

    fn start_editor_export(&mut self) -> Result<(), String> {
        let workspace = self
            .editor_workspace
            .as_ref()
            .ok_or_else(|| "No active editor project is available for export.".to_owned())?;
        if !workspace.asset_issues().is_empty() {
            return Err(format!(
                "Cannot export while the project has {} unresolved asset issue(s).",
                workspace.asset_issues().len()
            ));
        }
        let timeline_order: Vec<_> = workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect();
        let frame_selection = resolve_export_selection(
            self.editor_export_settings.frame_scope,
            &timeline_order,
            workspace.selection().selected(),
        )?;
        let options = build_project_export_options(&self.editor_export_settings, frame_selection)?;
        let output = validate_export_output(self.settings.output.trim())?;
        let snapshot = ProjectExportSnapshot::from_active(workspace.active_project());
        self.export_job
            .start(snapshot, output, options)
            .map_err(|error| error.to_string())?;
        self.notice = Some("GIF export started…".to_owned());
        Ok(())
    }

    fn receive_export_messages(&mut self) {
        let finished = self
            .export_job
            .drain()
            .into_iter()
            .any(|event| event == ExportJobEvent::Finished);
        if !finished {
            return;
        }
        let notice = self.export_job.take_result().map_or_else(
            || "GIF export worker finished without a result.".to_owned(),
            export_result_notice,
        );
        self.export_job = ExportJob::default();
        self.notice = Some(notice);
    }

    fn receive_open_project_messages(&mut self) {
        let finished = self
            .open_project_job
            .drain()
            .into_iter()
            .any(|event| event == OpenProjectJobEvent::Finished);
        if !finished {
            return;
        }
        let result = self.open_project_job.take_result();
        self.open_project_job = OpenProjectJob::default();
        self.notice = Some(match result {
            Some(Ok(opened)) => match self.activate_opened_project(opened) {
                Ok(notice) => notice,
                Err(error) => format!("Could not prepare opened project: {error}"),
            },
            Some(Err(error)) => format!("Could not open project: {error}"),
            None => "Project-open worker finished without a result.".to_owned(),
        });
    }

    fn receive_import_gif_messages(&mut self) {
        let finished = self
            .import_gif_job
            .drain()
            .into_iter()
            .any(|event| event == ImportGifJobEvent::Finished);
        if !finished {
            return;
        }
        let result = self.import_gif_job.take_result();
        self.import_gif_job = ImportGifJob::default();
        self.notice = Some(match result {
            Some(Ok(project)) => match self.activate_imported_gif(project) {
                Ok(notice) => notice,
                Err(error) => format!("Could not prepare imported GIF project: {error}"),
            },
            Some(Err(error)) => format!(
                "Could not import GIF: {error}. You can correct the path or file and retry."
            ),
            None => "GIF import worker finished without a result. You can retry safely.".to_owned(),
        });
    }

    fn activate_imported_gif(&mut self, project: ActiveProject) -> Result<String, String> {
        let source = Path::new(self.import_gif_path.trim());
        let output = edited_gif_path_for_import(source)?;
        let summary = activate_editor(&mut self.view, &mut self.editor_workspace, project)?;
        self.editor_ui_state = EditorUiState::default();
        self.editor_preview_cache = EditorPreviewCache::new();
        self.editor_export_settings = EditorExportSettings::default();
        self.export_job = ExportJob::default();
        self.settings.output = output.to_string_lossy().into_owned();
        Ok(format!(
            "Imported {} frame(s) into {}. Default GIF output is {} and will not overwrite the source.",
            summary.frames,
            summary.project_path.display(),
            output.display()
        ))
    }

    fn activate_opened_project(&mut self, opened: OpenedProject) -> Result<String, String> {
        let workspace = EditorWorkspace::from_opened(opened, EDITOR_HISTORY_LIMIT)
            .map_err(|error| error.to_string())?;
        let output = default_gif_path_for_project(workspace.project_root());
        let output_exists = output.exists();
        let notice = opened_project_notice(&workspace, &output, output_exists);

        self.editor_workspace = Some(workspace);
        self.editor_ui_state = EditorUiState::default();
        self.editor_preview_cache = EditorPreviewCache::new();
        self.editor_export_settings = EditorExportSettings::default();
        self.export_job = ExportJob::default();
        self.settings.output = output.to_string_lossy().into_owned();
        self.view = AppView::Editor;
        Ok(notice)
    }

    fn finish_recording_job(&mut self) {
        self.job = None;
        self.recording_countdown.cancel();
        self.recorder_overlay = None;
        self.restore_main_window = true;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EditorExportAction {
    #[default]
    None,
    Start,
    Cancel,
}

fn show_editor_scroll_area<R>(
    ui: &mut egui::Ui,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::containers::scroll_area::ScrollAreaOutput<R> {
    egui::ScrollArea::vertical()
        .id_salt("editor_page_vertical_scroll")
        .auto_shrink([false, false])
        .show(ui, contents)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "preview dimensions are capped below 1024 pixels before the egui f32 boundary"
)]
fn show_editor_preview_panel(
    ui: &mut egui::Ui,
    workspace: &EditorWorkspace,
    cache: &mut EditorPreviewCache,
) {
    ui.heading("Current frame preview");
    if !workspace.asset_issues().is_empty() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!(
                "Preview and export are blocked by {} unresolved asset issue(s).",
                workspace.asset_issues().len()
            ),
        );
        for issue in workspace.asset_issues() {
            ui.monospace(format!("{issue:?}"));
        }
        return;
    }
    let Some(frame_id) = workspace.selection().current() else {
        ui.label("Select a frame to preview it.");
        return;
    };
    match cache.preview(
        workspace.active_project(),
        frame_id,
        ui.ctx(),
        EDITOR_PREVIEW_MAX_SIZE,
    ) {
        Ok(preview) => {
            let natural = egui::vec2(
                preview.preview_size[0] as f32,
                preview.preview_size[1] as f32,
            );
            let available_width = ui.available_width().max(1.0);
            let scale = (available_width / natural.x).min(1.0);
            ui.image((preview.texture.id(), natural * scale));
            ui.weak(format!(
                "Rendered {}×{} · preview {}×{}",
                preview.rendered_size[0],
                preview.rendered_size[1],
                preview.preview_size[0],
                preview.preview_size[1]
            ));
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Could not render preview: {error}"),
            );
        }
    }
}

fn show_export_panel(
    ui: &mut egui::Ui,
    output: &mut String,
    settings: &mut EditorExportSettings,
    job: &ExportJob,
    selected_count: usize,
    asset_issue_count: usize,
) -> EditorExportAction {
    ui.heading("Export GIF");
    let active = export_job_is_active(job.state());
    ui.add_enabled_ui(!active, |ui| {
        show_export_configuration(ui, output, settings, selected_count);
    });
    if asset_issue_count > 0 {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Resolve {asset_issue_count} asset issue(s) before exporting."),
        );
    }

    match job.state() {
        ExportJobState::Idle => {
            if ui
                .add_enabled(asset_issue_count == 0, egui::Button::new("Export GIF"))
                .clicked()
            {
                EditorExportAction::Start
            } else {
                EditorExportAction::None
            }
        }
        ExportJobState::Running | ExportJobState::Cancelling => {
            if let Some(progress) = job.latest_progress() {
                ui.label(format!(
                    "{:?}: rendered {}/{}, encoded {}/{}",
                    progress.phase,
                    progress.frames_rendered,
                    progress.total_frames,
                    progress.frames_encoded,
                    progress.total_frames
                ));
            } else {
                ui.label("Starting export worker…");
            }
            if ui
                .add_enabled(
                    job.state() == ExportJobState::Running,
                    egui::Button::new(if job.state() == ExportJobState::Cancelling {
                        "Cancelling…"
                    } else {
                        "Cancel export"
                    }),
                )
                .clicked()
            {
                EditorExportAction::Cancel
            } else {
                EditorExportAction::None
            }
        }
        ExportJobState::Finished => {
            ui.label("Finishing export result…");
            EditorExportAction::None
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the compact two-column export form is kept together so every GIF option is auditable"
)]
fn show_export_configuration(
    ui: &mut egui::Ui,
    output: &mut String,
    settings: &mut EditorExportSettings,
    selected_count: usize,
) {
    egui::Grid::new("editor_export_configuration")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("Output");
            ui.text_edit_singleline(output);
            ui.end_row();

            ui.label("Frames");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut settings.frame_scope, ExportFrameScope::All, "All");
                ui.selectable_value(
                    &mut settings.frame_scope,
                    ExportFrameScope::Selected,
                    format!("Selected ({selected_count})"),
                );
            });
            ui.end_row();

            ui.label("Maximum colors");
            ui.add(egui::DragValue::new(&mut settings.max_colors).range(2..=256));
            ui.end_row();

            ui.label("Palette");
            egui::ComboBox::from_id_salt("editor_export_palette")
                .selected_text(match settings.palette {
                    ExportPaletteChoice::Local => "Local per frame",
                    ExportPaletteChoice::Global => "Global",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut settings.palette,
                        ExportPaletteChoice::Local,
                        "Local per frame",
                    );
                    ui.selectable_value(
                        &mut settings.palette,
                        ExportPaletteChoice::Global,
                        "Global",
                    );
                });
            ui.end_row();

            ui.label("Quantizer");
            egui::ComboBox::from_id_salt("editor_export_quantizer")
                .selected_text(export_quantizer_label(settings.quantizer))
                .show_ui(ui, |ui| {
                    for choice in [
                        ExportQuantizerChoice::MedianCut,
                        ExportQuantizerChoice::Octree,
                        ExportQuantizerChoice::Grayscale,
                        ExportQuantizerChoice::MostUsed,
                    ] {
                        ui.selectable_value(
                            &mut settings.quantizer,
                            choice,
                            export_quantizer_label(choice),
                        );
                    }
                });
            ui.end_row();

            ui.label("Dither");
            egui::ComboBox::from_id_salt("editor_export_dither")
                .selected_text(export_dither_label(settings.dither))
                .show_ui(ui, |ui| {
                    for choice in [
                        ExportDitherChoice::None,
                        ExportDitherChoice::Bayer,
                        ExportDitherChoice::FloydSteinberg,
                        ExportDitherChoice::Sierra,
                    ] {
                        ui.selectable_value(
                            &mut settings.dither,
                            choice,
                            export_dither_label(choice),
                        );
                    }
                });
            ui.end_row();

            ui.label("Alpha threshold");
            ui.add(egui::DragValue::new(&mut settings.alpha_threshold).range(0..=255));
            ui.end_row();

            ui.label("Loop");
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut settings.loop_choice,
                    ExportLoopChoice::Infinite,
                    "Infinite",
                );
                ui.selectable_value(
                    &mut settings.loop_choice,
                    ExportLoopChoice::Finite,
                    "Finite",
                );
                if settings.loop_choice == ExportLoopChoice::Finite {
                    ui.add(
                        egui::DragValue::new(&mut settings.finite_loop_count).range(1..=u16::MAX),
                    );
                }
            });
            ui.end_row();

            ui.label("Optimization");
            ui.horizontal(|ui| {
                ui.checkbox(&mut settings.delta, "Changed rectangles");
                ui.checkbox(&mut settings.overwrite, "Overwrite output");
            });
            ui.end_row();
        });
}

const fn export_quantizer_label(choice: ExportQuantizerChoice) -> &'static str {
    match choice {
        ExportQuantizerChoice::MedianCut => "Median cut",
        ExportQuantizerChoice::Octree => "Octree",
        ExportQuantizerChoice::Grayscale => "Grayscale",
        ExportQuantizerChoice::MostUsed => "Most used",
    }
}

const fn export_dither_label(choice: ExportDitherChoice) -> &'static str {
    match choice {
        ExportDitherChoice::None => "None",
        ExportDitherChoice::Bayer => "Bayer 4×4",
        ExportDitherChoice::FloydSteinberg => "Floyd–Steinberg",
        ExportDitherChoice::Sierra => "Sierra",
    }
}

fn draw_recorder_overlay(
    context: &egui::Context,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    source_geometry: PhysicalRect,
) -> RecorderOverlayFrame {
    let mut action = draw_recorder_toolbar(context, stage, progress);
    let central = egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::TRANSPARENT)
                .inner_margin(0),
        )
        .show(context, |ui| {
            let bounds = ui.max_rect();
            let capture = bounds.shrink(RECORDER_BORDER_POINTS);
            ui.painter().rect_stroke(
                capture,
                0.0,
                egui::Stroke::new(
                    RECORDER_BORDER_POINTS,
                    egui::Color32::from_rgb(242, 153, 74),
                ),
                egui::StrokeKind::Outside,
            );
            if stage.allows_moving() {
                let move_area = capture.shrink(18.0);
                let move_response = ui
                    .interact(
                        move_area,
                        ui.id().with("recorder-move-area"),
                        egui::Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::Move);
                move_recorder_window(ui.ctx(), &move_response);
            }
            if stage.allows_resizing() {
                add_recorder_resize_grips(ui, bounds);
            }
            capture
        })
        .inner;

    if context.input(|input| input.viewport().close_requested()) {
        action = RecorderOverlayAction::Close;
    }
    RecorderOverlayFrame {
        action,
        region: overlay_region_from_viewport(context, central, source_geometry),
    }
}

fn draw_recorder_toolbar(
    context: &egui::Context,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    egui::TopBottomPanel::bottom("recorder_controls")
        .exact_height(RECORDER_TOOLBAR_POINTS)
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(28, 30, 34))
                .inner_margin(8),
        )
        .show(context, |ui| match stage {
            RecorderStage::Ready => {
                ui.horizontal_centered(|ui| {
                    action = show_ready_recorder_controls(ui, context);
                });
            }
            RecorderStage::Countdown(remaining) => {
                ui.horizontal_centered(|ui| {
                    ui.strong(format!("Recording starts in {remaining}s"));
                    if ui.button("Cancel").clicked() {
                        action = RecorderOverlayAction::CancelCountdown;
                    }
                });
                ui.horizontal_centered(|ui| show_recorder_position_controls(ui, context));
            }
            RecorderStage::Recording => {
                ui.horizontal_centered(|ui| {
                    show_overlay_progress(ui, progress);
                    if ui.button("Pause").clicked() {
                        action = RecorderOverlayAction::Pause;
                    }
                    if ui.button("Stop").clicked() {
                        action = RecorderOverlayAction::Stop;
                    }
                    if ui.button("Discard").clicked() {
                        action = RecorderOverlayAction::Discard;
                    }
                });
                ui.horizontal_centered(|ui| show_recorder_position_controls(ui, context));
            }
            RecorderStage::Paused => {
                ui.horizontal_centered(|ui| {
                    ui.label("Paused");
                    if ui.button("Resume").clicked() {
                        action = RecorderOverlayAction::Resume;
                    }
                    if ui.button("Stop").clicked() {
                        action = RecorderOverlayAction::Stop;
                    }
                    if ui.button("Discard").clicked() {
                        action = RecorderOverlayAction::Discard;
                    }
                });
                ui.horizontal_centered(|ui| show_recorder_position_controls(ui, context));
            }
            RecorderStage::Finalizing => {
                ui.horizontal_centered(|ui| {
                    ui.spinner();
                    ui.label("Encoding GIF…");
                    if ui.button("Cancel").clicked() {
                        action = RecorderOverlayAction::Discard;
                    }
                });
            }
        });
    action
}

fn show_ready_recorder_controls(
    ui: &mut egui::Ui,
    context: &egui::Context,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    egui::Grid::new("ready_recorder_controls")
        .num_columns(6)
        .spacing([4.0, 3.0])
        .show(ui, |ui| {
            show_recorder_position_controls(ui, context);
            ui.end_row();

            if ui.small_button("W-").clicked() {
                nudge_recorder_size(context, -10.0, 0.0);
            }
            if ui.small_button("W+").clicked() {
                nudge_recorder_size(context, 10.0, 0.0);
            }
            if ui.small_button("H-").clicked() {
                nudge_recorder_size(context, 0.0, -10.0);
            }
            if ui.small_button("H+").clicked() {
                nudge_recorder_size(context, 0.0, 10.0);
            }
            if ui.button("Start").clicked() {
                action = RecorderOverlayAction::Start;
            }
            if ui.button("Cancel").clicked() {
                action = RecorderOverlayAction::Close;
            }
            ui.end_row();
        });
    action
}

fn show_recorder_position_controls(ui: &mut egui::Ui, context: &egui::Context) {
    let drag = ui.add(egui::Label::new("Move").sense(egui::Sense::drag()));
    move_recorder_window(context, &drag);
    if ui.small_button("X-").clicked() {
        nudge_recorder_window(context, -10.0, 0.0);
    }
    if ui.small_button("X+").clicked() {
        nudge_recorder_window(context, 10.0, 0.0);
    }
    if ui.small_button("Y-").clicked() {
        nudge_recorder_window(context, 0.0, -10.0);
    }
    if ui.small_button("Y+").clicked() {
        nudge_recorder_window(context, 0.0, 10.0);
    }
    ui.label("10 px");
}

fn show_overlay_progress(ui: &mut egui::Ui, progress: Option<WorkflowProgress>) {
    if let Some(progress) = progress {
        ui.label(format!(
            "{} frames · {:.1}s",
            progress.frames_captured,
            progress.capture_duration.as_secs_f32()
        ));
    } else {
        ui.label("Starting…");
    }
}

fn add_recorder_resize_grips(ui: &mut egui::Ui, bounds: egui::Rect) {
    let edge = 9.0;
    let corner = 18.0;
    let grips = [
        (
            egui::Rect::from_min_size(bounds.min, egui::vec2(corner, corner)),
            egui::ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNorthWest,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(bounds.max.x - corner, bounds.min.y),
                egui::vec2(corner, corner),
            ),
            egui::ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNorthEast,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(bounds.min.x, bounds.max.y - corner),
                egui::vec2(corner, corner),
            ),
            egui::ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeSouthWest,
        ),
        (
            egui::Rect::from_min_size(
                bounds.max - egui::vec2(corner, corner),
                egui::vec2(corner, corner),
            ),
            egui::ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeSouthEast,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(bounds.min.x + corner, bounds.min.y),
                egui::pos2(bounds.max.x - corner, bounds.min.y + edge),
            ),
            egui::ResizeDirection::North,
            egui::CursorIcon::ResizeNorth,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(bounds.min.x + corner, bounds.max.y - edge),
                egui::pos2(bounds.max.x - corner, bounds.max.y),
            ),
            egui::ResizeDirection::South,
            egui::CursorIcon::ResizeSouth,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(bounds.min.x, bounds.min.y + corner),
                egui::pos2(bounds.min.x + edge, bounds.max.y - corner),
            ),
            egui::ResizeDirection::West,
            egui::CursorIcon::ResizeWest,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(bounds.max.x - edge, bounds.min.y + corner),
                egui::pos2(bounds.max.x, bounds.max.y - corner),
            ),
            egui::ResizeDirection::East,
            egui::CursorIcon::ResizeEast,
        ),
    ];
    for (index, (rect, direction, cursor)) in grips.into_iter().enumerate() {
        let response = ui
            .interact(
                rect,
                ui.id().with(("recorder-resize", index)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(cursor);
        resize_recorder_window(ui.ctx(), &response, direction);
    }
}

fn move_recorder_window(context: &egui::Context, response: &egui::Response) {
    let pointer_down = context.input(|input| input.pointer.primary_down());
    if !(response.dragged() || response.hovered() && pointer_down) {
        return;
    }
    let delta = context.input(|input| input.pointer.delta());
    if delta == egui::Vec2::ZERO {
        return;
    }
    if let Some(position) = context.input(|input| input.viewport().outer_rect.map(|rect| rect.min))
    {
        context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position + delta));
    }
}

fn nudge_recorder_window(context: &egui::Context, horizontal: f32, vertical: f32) {
    let pixels_per_point = context.input(|input| {
        input
            .viewport()
            .native_pixels_per_point
            .unwrap_or_else(|| context.pixels_per_point())
    });
    let delta = egui::vec2(horizontal, vertical) / pixels_per_point;
    if let Some(position) = context.input(|input| input.viewport().outer_rect.map(|rect| rect.min))
    {
        context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position + delta));
    }
}

fn nudge_recorder_size(context: &egui::Context, horizontal: f32, vertical: f32) {
    let pixels_per_point = context.input(|input| {
        input
            .viewport()
            .native_pixels_per_point
            .unwrap_or_else(|| context.pixels_per_point())
    });
    let delta = egui::vec2(horizontal, vertical) / pixels_per_point;
    if let Some(size) = context.input(|input| input.viewport().inner_rect.map(|rect| rect.size())) {
        context.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            (size + delta).max(egui::vec2(180.0, 130.0)),
        ));
    }
}

fn resize_recorder_window(
    context: &egui::Context,
    response: &egui::Response,
    direction: egui::ResizeDirection,
) {
    let pointer_down = context.input(|input| input.pointer.primary_down());
    if !(response.dragged() || response.hovered() && pointer_down) {
        return;
    }
    let delta = context.input(|input| input.pointer.delta());
    if delta == egui::Vec2::ZERO {
        return;
    }
    let Some((outer, inner)) = context.input(|input| {
        let viewport = input.viewport();
        Some((viewport.outer_rect?, viewport.inner_rect?))
    }) else {
        return;
    };
    let mut position = outer.min;
    let mut size = inner.size();
    match direction {
        egui::ResizeDirection::North => {
            position.y += delta.y;
            size.y -= delta.y;
        }
        egui::ResizeDirection::South => size.y += delta.y,
        egui::ResizeDirection::East => size.x += delta.x,
        egui::ResizeDirection::West => {
            position.x += delta.x;
            size.x -= delta.x;
        }
        egui::ResizeDirection::NorthEast => {
            position.y += delta.y;
            size.y -= delta.y;
            size.x += delta.x;
        }
        egui::ResizeDirection::SouthEast => {
            size.x += delta.x;
            size.y += delta.y;
        }
        egui::ResizeDirection::NorthWest => {
            position += delta;
            size -= delta;
        }
        egui::ResizeDirection::SouthWest => {
            position.x += delta.x;
            size.x -= delta.x;
            size.y += delta.y;
        }
    }
    let minimum = egui::vec2(180.0, 130.0);
    size = size.max(minimum);
    context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
    context.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn overlay_region_from_viewport(
    context: &egui::Context,
    capture_rect: egui::Rect,
    source_geometry: PhysicalRect,
) -> Option<PhysicalRect> {
    let (outer, pixels_per_point) = context.input(|input| {
        let viewport = input.viewport();
        Some((viewport.outer_rect?, viewport.native_pixels_per_point?))
    })?;
    let absolute_left = ((outer.min.x + capture_rect.min.x) * pixels_per_point).round() as i64;
    let absolute_top = ((outer.min.y + capture_rect.min.y) * pixels_per_point).round() as i64;
    let width = (capture_rect.width() * pixels_per_point).round().max(1.0) as u32;
    let height = (capture_rect.height() * pixels_per_point).round().max(1.0) as u32;
    let local_left = absolute_left - i64::from(source_geometry.origin().x);
    let local_top = absolute_top - i64::from(source_geometry.origin().y);
    let region = PhysicalRect::new(
        i32::try_from(local_left).ok()?,
        i32::try_from(local_top).ok()?,
        width,
        height,
    )
    .ok()?;
    region.fits_within(source_geometry.size()).then_some(region)
}

fn frame_to_preview(frame: &CapturedFrame) -> Result<(egui::ColorImage, u32, u32), String> {
    const MAX_PREVIEW_WIDTH: u32 = 1_600;
    const MAX_PREVIEW_HEIGHT: u32 = 900;

    let source_width = frame.size().width();
    let source_height = frame.size().height();
    let source = tightly_packed_rgba(frame)?;
    let (preview_width, preview_height) = fit_dimensions(
        source_width,
        source_height,
        MAX_PREVIEW_WIDTH,
        MAX_PREVIEW_HEIGHT,
    );
    let preview = if (preview_width, preview_height) == (source_width, source_height) {
        source
    } else {
        resize_nearest_rgba(
            &source,
            source_width,
            source_height,
            preview_width,
            preview_height,
        )?
    };
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [
            usize::try_from(preview_width).map_err(|_| "preview width is too large")?,
            usize::try_from(preview_height).map_err(|_| "preview height is too large")?,
        ],
        &preview,
    );
    Ok((image, source_width, source_height))
}

fn tightly_packed_rgba(frame: &CapturedFrame) -> Result<Vec<u8>, String> {
    let width = usize::try_from(frame.size().width()).map_err(|_| "frame width is too large")?;
    let height = usize::try_from(frame.size().height()).map_err(|_| "frame height is too large")?;
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "frame row length overflowed".to_owned())?;
    let expected = row_bytes
        .checked_mul(height)
        .ok_or_else(|| "frame byte length overflowed".to_owned())?;
    let mut pixels = Vec::with_capacity(expected);
    for row in frame.pixels().chunks(frame.stride()).take(height) {
        let row = row
            .get(..row_bytes)
            .ok_or_else(|| "captured preview row is truncated".to_owned())?;
        match frame.format() {
            PixelFormat::Rgba8 => pixels.extend_from_slice(row),
            PixelFormat::Bgra8 => {
                for pixel in row.as_chunks::<4>().0 {
                    pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
            _ => return Err("unsupported future preview pixel format".into()),
        }
    }
    if pixels.len() != expected {
        return Err("captured preview has too few rows".into());
    }
    Ok(pixels)
}

fn fit_dimensions(width: u32, height: u32, maximum_width: u32, maximum_height: u32) -> (u32, u32) {
    if width <= maximum_width && height <= maximum_height {
        return (width, height);
    }
    if u64::from(width) * u64::from(maximum_height) >= u64::from(height) * u64::from(maximum_width)
    {
        let scaled_height =
            (u64::from(height) * u64::from(maximum_width) / u64::from(width)).max(1);
        (
            maximum_width,
            u32::try_from(scaled_height).unwrap_or(maximum_height),
        )
    } else {
        let scaled_width =
            (u64::from(width) * u64::from(maximum_height) / u64::from(height)).max(1);
        (
            u32::try_from(scaled_width).unwrap_or(maximum_width),
            maximum_height,
        )
    }
}

fn resize_nearest_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) -> Result<Vec<u8>, String> {
    let output_len = usize::try_from(u64::from(output_width) * u64::from(output_height) * 4)
        .map_err(|_| "preview output size is too large")?;
    let mut output = vec![0; output_len];
    for output_y in 0..output_height {
        let source_y = u64::from(output_y) * u64::from(source_height) / u64::from(output_height);
        for output_x in 0..output_width {
            let source_x = u64::from(output_x) * u64::from(source_width) / u64::from(output_width);
            let source_pixel = usize::try_from((source_y * u64::from(source_width) + source_x) * 4)
                .map_err(|_| "preview source offset overflowed")?;
            let output_pixel = usize::try_from(
                (u64::from(output_y) * u64::from(output_width) + u64::from(output_x)) * 4,
            )
            .map_err(|_| "preview output offset overflowed")?;
            output[output_pixel..output_pixel + 4]
                .copy_from_slice(&source[source_pixel..source_pixel + 4]);
        }
    }
    Ok(output)
}

fn clamp_to_rect(position: egui::Pos2, bounds: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        position.x.clamp(bounds.min.x, bounds.max.x),
        position.y.clamp(bounds.min.y, bounds.max.y),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn map_preview_selection(
    preview: egui::Rect,
    selection: egui::Rect,
    source_width: u32,
    source_height: u32,
) -> Option<PhysicalRect> {
    if preview.width() <= 0.0
        || preview.height() <= 0.0
        || selection.width() < 1.0
        || selection.height() < 1.0
    {
        return None;
    }
    let left = ((selection.min.x - preview.min.x) / preview.width() * source_width as f32)
        .floor()
        .clamp(0.0, source_width as f32) as u32;
    let top = ((selection.min.y - preview.min.y) / preview.height() * source_height as f32)
        .floor()
        .clamp(0.0, source_height as f32) as u32;
    let right = ((selection.max.x - preview.min.x) / preview.width() * source_width as f32)
        .ceil()
        .clamp(0.0, source_width as f32) as u32;
    let bottom = ((selection.max.y - preview.min.y) / preview.height() * source_height as f32)
        .ceil()
        .clamp(0.0, source_height as f32) as u32;
    PhysicalRect::new(
        i32::try_from(left).ok()?,
        i32::try_from(top).ok()?,
        right.checked_sub(left)?,
        bottom.checked_sub(top)?,
    )
    .ok()
}

fn apply_overlay_region(
    settings: &mut RecordingSettings,
    stage: RecorderStage,
    region: PhysicalRect,
) {
    if !stage.allows_moving() {
        return;
    }
    settings.region_enabled = true;
    settings.region_x = region.origin().x;
    settings.region_y = region.origin().y;
    if stage.allows_resizing() {
        settings.region_width = region.size().width();
        settings.region_height = region.size().height();
    }
}

const fn should_sync_retarget(stage: RecorderStage, action: RecorderOverlayAction) -> bool {
    stage.allows_retargeting()
        && !matches!(
            action,
            RecorderOverlayAction::Stop
                | RecorderOverlayAction::Discard
                | RecorderOverlayAction::Close
        )
}

fn project_path_for_output(output: &Path) -> Result<PathBuf, String> {
    output
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "Output must have a filename stem for its project directory.".to_owned())?;
    Ok(output.with_extension("gfsproj"))
}

fn resolve_export_selection(
    scope: ExportFrameScope,
    timeline_order: &[FrameId],
    selected: &BTreeSet<FrameId>,
) -> Result<ProjectFrameSelection, String> {
    if timeline_order.is_empty() {
        return Err("The project has no frames to export.".to_owned());
    }
    match scope {
        ExportFrameScope::All => Ok(ProjectFrameSelection::All),
        ExportFrameScope::Selected => {
            let ordered: Vec<_> = timeline_order
                .iter()
                .copied()
                .filter(|frame_id| selected.contains(frame_id))
                .collect();
            if ordered.is_empty() {
                return Err(
                    "Select at least one frame before exporting Selected frames.".to_owned(),
                );
            }
            Ok(ProjectFrameSelection::Ordered(ordered))
        }
    }
}

fn build_project_export_options(
    settings: &EditorExportSettings,
    frames: ProjectFrameSelection,
) -> Result<ProjectGifExportOptions, String> {
    if !(2..=256).contains(&settings.max_colors) {
        return Err("Maximum colors must be between 2 and 256.".to_owned());
    }
    if settings.loop_choice == ExportLoopChoice::Finite && settings.finite_loop_count == 0 {
        return Err("Finite loop count must be at least one.".to_owned());
    }
    let palette_mode = match settings.palette {
        ExportPaletteChoice::Local => PaletteMode::LocalPerFrame,
        ExportPaletteChoice::Global => PaletteMode::Global,
    };
    let quantizer = match settings.quantizer {
        ExportQuantizerChoice::MedianCut => QuantizerStrategy::MedianCut,
        ExportQuantizerChoice::Octree => QuantizerStrategy::Octree,
        ExportQuantizerChoice::Grayscale => QuantizerStrategy::Grayscale,
        ExportQuantizerChoice::MostUsed => QuantizerStrategy::MostUsed,
    };
    let dither = match settings.dither {
        ExportDitherChoice::None => DitherMode::None,
        ExportDitherChoice::Bayer => DitherMode::Bayer4x4,
        ExportDitherChoice::FloydSteinberg => DitherMode::FloydSteinberg,
        ExportDitherChoice::Sierra => DitherMode::Sierra,
    };
    let loop_behavior = match settings.loop_choice {
        ExportLoopChoice::Infinite => LoopBehavior::Infinite,
        ExportLoopChoice::Finite => LoopBehavior::Finite(settings.finite_loop_count),
    };
    Ok(ProjectGifExportOptions {
        frames,
        encoding: EncodeOptions {
            max_colors: settings.max_colors,
            loop_behavior,
            transparency: Transparency::AlphaThreshold(settings.alpha_threshold),
            palette_mode,
            quantizer,
            delta_mode: if settings.delta {
                DeltaMode::ChangedRectangles
            } else {
                DeltaMode::FullFrames
            },
            dither,
            ..EncodeOptions::default()
        },
        overwrite_existing: settings.overwrite,
        ..ProjectGifExportOptions::default()
    })
}

fn validate_export_output(output: &str) -> Result<PathBuf, String> {
    let output = PathBuf::from(output.trim());
    if output.file_name().is_none() {
        return Err("Export output must identify a GIF file.".to_owned());
    }
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("gif"))
    {
        return Err("Export output filename must end in .gif.".to_owned());
    }
    Ok(output)
}

const fn export_job_is_active(state: ExportJobState) -> bool {
    matches!(state, ExportJobState::Running | ExportJobState::Cancelling)
}

const fn can_navigate_back(
    view: AppView,
    open_state: OpenProjectJobState,
    import_state: ImportGifJobState,
) -> bool {
    !((matches!(view, AppView::OpenProject) && matches!(open_state, OpenProjectJobState::Running))
        || (matches!(view, AppView::ImportGif)
            && matches!(import_state, ImportGifJobState::Running)))
}

fn export_result_notice(result: Result<ProjectGifExportReport, ExportJobError>) -> String {
    match result {
        Ok(report) => format!(
            "Exported {} selected frames as {} GIF images ({} bytes) to {}",
            report.selected_frames,
            report.encoding.encoded_frames,
            report.bytes_written,
            report.output_path.display()
        ),
        Err(error) => format!("GIF export failed: {error}"),
    }
}

fn default_gif_path_for_project(project_root: &Path) -> PathBuf {
    project_root.with_extension("gif")
}

fn edited_gif_path_for_import(source: &Path) -> Result<PathBuf, String> {
    let stem = source
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "Imported GIF path has no filename stem.".to_owned())?;
    let mut filename = stem.to_os_string();
    filename.push("-edited.gif");
    Ok(source.with_file_name(filename))
}

fn opened_project_notice(
    workspace: &EditorWorkspace,
    output: &Path,
    output_exists: bool,
) -> String {
    let mut details = vec![format!(
        "Opened {} frame(s) from {}.",
        workspace.manifest().timeline.frames.len(),
        workspace.project_root().display()
    )];
    if let Some(recovery) = workspace.journal_recovery() {
        if recovery.replayed_records > 0 {
            details.push(format!(
                "Recovered {} journal edit(s); the manifest snapshot should be checkpointed.",
                recovery.replayed_records
            ));
        }
        if !recovery.is_clean() {
            details.push(format!(
                "Journal recovery stopped at a non-clean tail ({:?}); repair is required before editing.",
                recovery.stop_reason
            ));
        }
    }
    if !workspace.asset_issues().is_empty() {
        details.push(format!(
            "Found {} asset issue(s); affected previews and GIF export remain unavailable.",
            workspace.asset_issues().len()
        ));
    }
    if output_exists {
        details.push(format!(
            "The default GIF {} already exists; enable Overwrite output to replace it.",
            output.display()
        ));
    }
    details.join(" ")
}

fn activate_editor(
    view: &mut AppView,
    editor_workspace: &mut Option<EditorWorkspace>,
    project: ActiveProject,
) -> Result<CompletedProjectSummary, String> {
    let mut workspace = EditorWorkspace::from_active(project, EDITOR_HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    let frames = workspace.manifest().timeline.frames.len();
    if frames > 0 {
        workspace
            .select_first()
            .map_err(|error| error.to_string())?;
    }
    let duration_us = workspace
        .manifest()
        .timeline
        .total_duration()
        .ok_or_else(|| "recorded project duration overflowed".to_owned())?
        .get();
    let project_path = workspace.project_root().to_path_buf();
    *editor_workspace = Some(workspace);
    *view = AppView::Editor;
    Ok(CompletedProjectSummary {
        frames,
        duration_us,
        project_path,
    })
}

fn remove_completed_project(project: ActiveProject) -> Result<(), String> {
    let project_path = project.layout().root.clone();
    if project_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("gfsproj")
    {
        return Err(format!(
            "refused to remove unexpected project path {}",
            project_path.display()
        ));
    }
    drop(project);
    fs::remove_dir_all(&project_path).map_err(|error| {
        format!(
            "could not remove discarded project {}: {error}",
            project_path.display()
        )
    })
}

fn validate_settings(settings: &RecordingSettings) -> Result<(), String> {
    if settings.duration_ms > MAX_RECORDING_DURATION_MS {
        return Err(format!(
            "Maximum duration must be 0 (manual stop) or at most {MAX_RECORDING_DURATION_MS} ms."
        ));
    }
    if !(1..=60).contains(&settings.fps) {
        return Err("FPS must be between 1 and 60.".into());
    }
    if settings.countdown_seconds > MAX_COUNTDOWN_SECONDS {
        return Err(format!(
            "Countdown must be between 0 and {MAX_COUNTDOWN_SECONDS} seconds."
        ));
    }
    let output = Path::new(settings.output.trim());
    if output.file_name().is_none() {
        return Err("Output must identify a GIF file.".into());
    }
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("gif"))
    {
        return Err("Output filename must end in .gif.".into());
    }
    if output.exists() {
        return Err("Output already exists; choose a different filename.".into());
    }
    let project_path = project_path_for_output(output)?;
    if project_path
        .try_exists()
        .map_err(|error| format!("Could not inspect project path: {error}"))?
    {
        return Err(format!(
            "Project path {} already exists; choose a different output filename.",
            project_path.display()
        ));
    }
    Ok(())
}

fn collect_x11_recording(
    worker: &RecordingWorkerRequest,
    control: &mut RecordingControl,
    cancellation: &CancellationFlag,
    progress: &mut dyn gif_from_screen_workflow::WorkflowProgressSink,
) -> Result<CollectedRecording, Box<dyn std::error::Error + Send + Sync>> {
    let backend = X11CaptureBackend::connect(None)?;
    let target = if worker.settings.region_enabled {
        CaptureTarget::Region {
            source: worker.source_id.clone(),
            region: PhysicalRect::new(
                worker.settings.region_x,
                worker.settings.region_y,
                worker.settings.region_width,
                worker.settings.region_height,
            )?,
        }
    } else {
        match worker.source_kind {
            CaptureSourceKind::Monitor => CaptureTarget::Monitor(worker.source_id.clone()),
            CaptureSourceKind::Window => CaptureTarget::Window(worker.source_id.clone()),
            _ => return Err("unsupported future X11 capture source kind".into()),
        }
    };
    let mut request = CaptureRequest::new(target, CaptureCadence::fixed_fps(worker.settings.fps)?);
    request.cursor = CursorCaptureMode::Embedded;
    collect_controlled(
        &backend,
        request,
        &collection_options(&worker.settings),
        control,
        cancellation,
        progress,
    )
    .map_err(Into::into)
}

fn persist_recording_project(
    worker: &RecordingWorkerRequest,
    recording: CollectedRecording,
    cancellation: &CancellationFlag,
) -> Result<ActiveProject, Box<dyn std::error::Error + Send + Sync>> {
    ensure_recording_not_cancelled(cancellation)?;
    if Path::new(worker.settings.output.trim()).try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "final GIF target appeared while recording",
        )
        .into());
    }
    if worker.project_path.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "project directory appeared while recording",
        )
        .into());
    }
    let frame_ids = (0..recording.frames().len())
        .map(|_| FrameId::from_u128(Uuid::new_v4().as_u128()))
        .collect();
    let created_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let created_millis = i64::try_from(created_millis)
        .map_err(|_| io::Error::other("current time does not fit the project timestamp"))?;
    let project = persist_collected_recording(
        &worker.project_path,
        recording,
        RecordingProjectOptions {
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            frame_ids,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at: UnixTimeMs::new(created_millis),
            source_label: Some(worker.source_label.clone()),
        },
    )?;
    Ok(project)
}

fn ensure_recording_not_cancelled(
    cancellation: &CancellationFlag,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if cancellation.is_cancelled() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "recording was discarded").into())
    } else {
        Ok(())
    }
}

fn collection_options(settings: &RecordingSettings) -> CollectOptions {
    CollectOptions {
        limit: collection_limit(settings.duration_ms),
        frame_retention: frame_retention(settings.changes_only),
        tail_frame_duration: Duration::from_micros(1_000_000 / u64::from(settings.fps)),
        ..CollectOptions::default()
    }
}

fn collection_limit(duration_ms: u64) -> CollectionLimit {
    if duration_ms == 0 {
        CollectionLimit::UntilStopped
    } else {
        CollectionLimit::Duration(Duration::from_millis(duration_ms))
    }
}

const fn frame_retention(changes_only: bool) -> FrameRetention {
    if changes_only {
        FrameRetention::ChangesOnly
    } else {
        FrameRetention::All
    }
}

fn load_x11_sources() -> Result<Vec<CaptureSource>, String> {
    let backend = X11CaptureBackend::connect(None)
        .map_err(|error| format!("Could not connect to X11: {error}"))?;
    let sources = backend
        .list_sources()
        .map_err(|error| format!("Could not enumerate X11 sources: {error}"))?;
    if sources.is_empty() {
        Err("X11 did not report any capture sources.".into())
    } else {
        Ok(sources)
    }
}

fn landing_action(ui: &mut egui::Ui, title: &str, description: &str, enabled: bool) -> bool {
    let mut clicked = false;
    ui.group(|ui| {
        ui.set_min_height(112.0);
        ui.set_min_width(LANDING_CARD_MIN_WIDTH);
        clicked = ui.add_enabled(enabled, egui::Button::new(title)).clicked();
        ui.label(description);
        if !enabled {
            ui.weak("Planned");
        }
    });
    clicked
}

#[cfg(test)]
fn landing_cards_fit(available_width: f32, column_spacing: f32) -> bool {
    available_width >= LANDING_CARD_MIN_WIDTH * 2.0 + column_spacing
}

fn parse_startup_intent(arguments: impl IntoIterator<Item = OsString>) -> StartupIntent {
    let mut arguments = arguments.into_iter();
    let Some(first) = arguments.next() else {
        return StartupIntent::None;
    };
    let (kind, path) = if first == OsStr::new("--project") {
        let Some(path) = arguments.next() else {
            return StartupIntent::Invalid("--project requires a .gfsproj path".to_owned());
        };
        (StartupIntentKind::Project, PathBuf::from(path))
    } else if first == OsStr::new("--import-gif") {
        let Some(path) = arguments.next() else {
            return StartupIntent::Invalid("--import-gif requires a .gif path".to_owned());
        };
        (StartupIntentKind::Gif, PathBuf::from(path))
    } else if first.to_string_lossy().starts_with('-') {
        return StartupIntent::Invalid(format!(
            "Unknown desktop argument '{}'. Use --project PATH or --import-gif PATH.",
            first.to_string_lossy()
        ));
    } else {
        let path = PathBuf::from(first);
        let kind = if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
        {
            StartupIntentKind::Gif
        } else {
            StartupIntentKind::Project
        };
        (kind, path)
    };
    if let Some(unexpected) = arguments.next() {
        return StartupIntent::Invalid(format!(
            "Unexpected extra desktop argument '{}'.",
            unexpected.to_string_lossy()
        ));
    }
    match kind {
        StartupIntentKind::Project => StartupIntent::OpenProject(path),
        StartupIntentKind::Gif => StartupIntent::ImportGif(path),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupIntentKind {
    Project,
    Gif,
}

fn main() -> eframe::Result {
    let startup_intent = parse_startup_intent(std::env::args_os().skip(1));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([820.0, 560.0])
            .with_min_inner_size([680.0, 440.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |_creation_context| {
            let mut app = GifFromScreenApp::default();
            app.apply_startup_intent(startup_intent);
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use eframe::egui;
    use gif_from_screen_application::{ProjectFrameSelection, ProjectGifExportReport};
    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize as DomainSize,
        ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, UnixTimeMs,
    };
    use gif_from_screen_gif::{
        BuiltinGifEncoder, DeltaMode, DitherMode, EncodeOptions, EncodeReport, LoopBehavior,
        PaletteMode, QuantizerStrategy, RgbaFrame, Transparency,
    };
    use gif_from_screen_project::ActiveProject;
    use tempfile::tempdir;

    use super::{
        AppView, EDITOR_PREVIEW_MAX_SIZE, EditorExportSettings, ExportDitherChoice,
        ExportFrameScope, ExportLoopChoice, ExportPaletteChoice, ExportQuantizerChoice,
        GifFromScreenApp, MAX_COUNTDOWN_SECONDS, MAX_RECORDING_DURATION_MS, RecorderOverlayAction,
        RecorderStage, RecordingSettings, StartupIntent, activate_editor, apply_overlay_region,
        build_project_export_options, can_navigate_back, collection_limit, collection_options,
        default_gif_path_for_project, edited_gif_path_for_import, export_job_is_active,
        export_result_notice, fit_dimensions, frame_retention, landing_cards_fit,
        map_preview_selection, parse_startup_intent, project_path_for_output,
        remove_completed_project, resize_nearest_rgba, resolve_export_selection,
        should_sync_retarget, show_editor_scroll_area, validate_export_output, validate_settings,
    };
    use crate::editor_workspace::EditorWorkspace;
    use crate::export_job::{ExportJobError, ExportJobState};
    use crate::import_gif_job::ImportGifJobState;
    use crate::open_project_job::OpenProjectJobState;
    use gif_from_screen_workflow::{CollectionLimit, FrameRetention};

    #[test]
    fn validates_recording_bounds_and_extension() {
        let directory = tempdir().unwrap();
        let mut settings = RecordingSettings {
            output: directory
                .path()
                .join("gfs-ui-validation.gif")
                .to_string_lossy()
                .into_owned(),
            ..RecordingSettings::default()
        };
        assert!(validate_settings(&settings).is_ok());
        settings.countdown_seconds = MAX_COUNTDOWN_SECONDS;
        assert!(validate_settings(&settings).is_ok());
        settings.countdown_seconds = MAX_COUNTDOWN_SECONDS + 1;
        assert!(validate_settings(&settings).is_err());
        settings.countdown_seconds = 0;
        assert!(validate_settings(&settings).is_ok());
        settings.duration_ms = MAX_RECORDING_DURATION_MS;
        assert!(validate_settings(&settings).is_ok());
        settings.duration_ms = MAX_RECORDING_DURATION_MS + 1;
        assert!(validate_settings(&settings).is_err());
        settings.duration_ms = 0;
        assert!(validate_settings(&settings).is_ok());
        settings.fps = 0;
        assert!(validate_settings(&settings).is_err());
        settings.fps = 10;
        settings.output = "capture.mp4".into();
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn desktop_startup_arguments_route_projects_and_gifs_without_flag_guessing() {
        assert_eq!(
            parse_startup_intent(Vec::<OsString>::new()),
            StartupIntent::None
        );
        assert_eq!(
            parse_startup_intent(["--project", "/tmp/demo.gfsproj"].map(OsString::from)),
            StartupIntent::OpenProject(PathBuf::from("/tmp/demo.gfsproj"))
        );
        assert_eq!(
            parse_startup_intent(["--import-gif", "/tmp/demo.data"].map(OsString::from)),
            StartupIntent::ImportGif(PathBuf::from("/tmp/demo.data"))
        );
        assert_eq!(
            parse_startup_intent([OsString::from("/tmp/demo.GIF")]),
            StartupIntent::ImportGif(PathBuf::from("/tmp/demo.GIF"))
        );
        assert_eq!(
            parse_startup_intent([OsString::from("/tmp/demo.gfsproj")]),
            StartupIntent::OpenProject(PathBuf::from("/tmp/demo.gfsproj"))
        );
        assert!(matches!(
            parse_startup_intent([OsString::from("--unknown")]),
            StartupIntent::Invalid(message) if message.contains("Unknown desktop argument")
        ));
        assert!(matches!(
            parse_startup_intent(["--project", "one", "two"].map(OsString::from)),
            StartupIntent::Invalid(message) if message.contains("Unexpected extra")
        ));
    }

    #[test]
    fn derives_project_path_and_rejects_existing_project_before_countdown() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("capture.demo.gif");
        let project = directory.path().join("capture.demo.gfsproj");
        assert_eq!(project_path_for_output(&output).unwrap(), project);

        let settings = RecordingSettings {
            output: output.to_string_lossy().into_owned(),
            ..RecordingSettings::default()
        };
        assert!(validate_settings(&settings).is_ok());
        fs::create_dir(&project).unwrap();
        let error = validate_settings(&settings).unwrap_err();
        assert!(error.contains("already exists"));
        assert!(error.contains("capture.demo.gfsproj"));
    }

    #[test]
    fn validation_rejects_an_existing_final_gif_independently() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("existing.gif");
        fs::write(&output, b"existing GIF").unwrap();
        let settings = RecordingSettings {
            output: output.to_string_lossy().into_owned(),
            ..RecordingSettings::default()
        };

        let error = validate_settings(&settings).unwrap_err();

        assert!(error.contains("Output already exists"));
        assert!(!directory.path().join("existing.gfsproj").exists());
    }

    #[test]
    fn collection_limit_and_retention_follow_recording_settings() {
        assert!(matches!(collection_limit(0), CollectionLimit::UntilStopped));
        assert!(matches!(
            collection_limit(2_500),
            CollectionLimit::Duration(duration) if duration == Duration::from_millis(2_500)
        ));
        assert_eq!(frame_retention(false), FrameRetention::All);
        assert_eq!(frame_retention(true), FrameRetention::ChangesOnly);

        let settings = RecordingSettings {
            duration_ms: 0,
            fps: 20,
            changes_only: true,
            ..RecordingSettings::default()
        };
        let options = collection_options(&settings);
        assert!(matches!(options.limit, CollectionLimit::UntilStopped));
        assert_eq!(options.frame_retention, FrameRetention::ChangesOnly);
        assert_eq!(options.tail_frame_duration, Duration::from_millis(50));
    }

    #[test]
    fn selected_export_frames_follow_timeline_order_not_identity_order() {
        let timeline_order = [
            FrameId::from_u128(30),
            FrameId::from_u128(10),
            FrameId::from_u128(20),
        ];
        let selected = BTreeSet::from([FrameId::from_u128(20), FrameId::from_u128(30)]);

        let resolved =
            resolve_export_selection(ExportFrameScope::Selected, &timeline_order, &selected)
                .unwrap();

        assert_eq!(
            resolved,
            ProjectFrameSelection::Ordered(vec![FrameId::from_u128(30), FrameId::from_u128(20)])
        );
        assert!(matches!(
            resolve_export_selection(ExportFrameScope::All, &timeline_order, &selected),
            Ok(ProjectFrameSelection::All)
        ));
        assert!(
            resolve_export_selection(
                ExportFrameScope::Selected,
                &timeline_order,
                &BTreeSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn editor_export_settings_map_to_application_and_encoder_options() {
        let settings = EditorExportSettings {
            frame_scope: ExportFrameScope::Selected,
            max_colors: 128,
            palette: ExportPaletteChoice::Global,
            quantizer: ExportQuantizerChoice::Octree,
            dither: ExportDitherChoice::Sierra,
            delta: true,
            alpha_threshold: 42,
            loop_choice: ExportLoopChoice::Finite,
            finite_loop_count: 7,
            overwrite: true,
        };
        let frames = ProjectFrameSelection::Ordered(vec![FrameId::from_u128(1)]);

        let options = build_project_export_options(&settings, frames.clone()).unwrap();

        assert_eq!(options.frames, frames);
        assert_eq!(options.encoding.max_colors, 128);
        assert_eq!(options.encoding.palette_mode, PaletteMode::Global);
        assert_eq!(options.encoding.quantizer, QuantizerStrategy::Octree);
        assert_eq!(options.encoding.dither, DitherMode::Sierra);
        assert_eq!(options.encoding.delta_mode, DeltaMode::ChangedRectangles);
        assert_eq!(
            options.encoding.transparency,
            Transparency::AlphaThreshold(42)
        );
        assert_eq!(options.encoding.loop_behavior, LoopBehavior::Finite(7));
        assert!(options.overwrite_existing);

        for (choice, expected) in [
            (
                ExportQuantizerChoice::MedianCut,
                QuantizerStrategy::MedianCut,
            ),
            (
                ExportQuantizerChoice::Grayscale,
                QuantizerStrategy::Grayscale,
            ),
            (ExportQuantizerChoice::MostUsed, QuantizerStrategy::MostUsed),
        ] {
            let mapped = build_project_export_options(
                &EditorExportSettings {
                    quantizer: choice,
                    ..EditorExportSettings::default()
                },
                ProjectFrameSelection::All,
            )
            .unwrap();
            assert_eq!(mapped.encoding.quantizer, expected);
        }
        for (choice, expected) in [
            (ExportDitherChoice::None, DitherMode::None),
            (ExportDitherChoice::Bayer, DitherMode::Bayer4x4),
            (
                ExportDitherChoice::FloydSteinberg,
                DitherMode::FloydSteinberg,
            ),
        ] {
            let mapped = build_project_export_options(
                &EditorExportSettings {
                    dither: choice,
                    ..EditorExportSettings::default()
                },
                ProjectFrameSelection::All,
            )
            .unwrap();
            assert_eq!(mapped.encoding.dither, expected);
        }
    }

    #[test]
    fn export_state_and_result_handling_are_explicit() {
        assert!(!export_job_is_active(ExportJobState::Idle));
        assert!(export_job_is_active(ExportJobState::Running));
        assert!(export_job_is_active(ExportJobState::Cancelling));
        assert!(!export_job_is_active(ExportJobState::Finished));
        let failure = export_result_notice(Err(ExportJobError::WorkerExited));
        assert!(failure.contains("worker exited"));

        let output = PathBuf::from("finished.gif");
        let success = export_result_notice(Ok(ProjectGifExportReport {
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::new(2),
            selected_frames: 3,
            encoding: EncodeReport {
                input_frames: 3,
                encoded_frames: 2,
                ..EncodeReport::default()
            },
            output_path: output.clone(),
            bytes_written: 99,
        }));
        assert!(success.contains("3 selected frames"));
        assert!(success.contains("2 GIF images"));
        assert!(success.contains("99 bytes"));
        assert!(success.contains(output.to_string_lossy().as_ref()));
        assert_eq!(validate_export_output(" finished.gif ").unwrap(), output);
        assert!(validate_export_output("finished.mp4").is_err());
    }

    #[test]
    fn editor_page_scroll_keeps_controls_below_the_preview_reachable() {
        let context = egui::Context::default();
        let mut metrics = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(820.0, 560.0),
            )),
            ..egui::RawInput::default()
        };
        let preview_height = f32::from(u16::try_from(EDITOR_PREVIEW_MAX_SIZE[1]).unwrap());

        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let output = show_editor_scroll_area(ui, |ui| {
                    ui.allocate_space(egui::vec2(1.0, preview_height + 500.0));
                    "export-controls-rendered"
                });
                metrics = Some((
                    output.content_size.y,
                    output.inner_rect.height(),
                    output.inner,
                ));
            });
        });

        let (content_height, viewport_height, marker) = metrics.unwrap();
        assert_eq!(marker, "export-controls-rendered");
        assert!(content_height > viewport_height);
    }

    fn empty_project(root: &Path) -> ActiveProject {
        ActiveProject::create(
            root,
            ProjectManifest::new(
                ProjectId::from_u128(1),
                "desktop-test",
                UnixTimeMs::new(1),
                Canvas {
                    size: DomainSize::new(1, 1).unwrap(),
                    color_space: ColorSpace::Srgb,
                    background: CanvasBackground::Transparent,
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn single_frame_project(root: &Path) -> ActiveProject {
        let mut project = empty_project(root);
        let pixels = [12, 34, 56, 255];
        let asset_id = project.assets().put(&pixels).unwrap();
        project
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: 4,
                            kind: AssetKind::Frame {
                                size: DomainSize::new(1, 1).unwrap(),
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: vec![FrameClip {
                            id: FrameId::from_u128(7),
                            asset_id,
                            duration: DurationUs::new(10_000).unwrap(),
                            transform: ClipTransform::default(),
                            capture_metadata: CaptureMetadata::default(),
                            effects: Vec::new(),
                        }],
                    },
                ],
            })
            .unwrap();
        project.checkpoint_and_compact().unwrap();
        project
    }

    fn drain_open_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.open_project_job.state() == OpenProjectJobState::Running {
            app.receive_open_project_messages();
            assert!(Instant::now() < deadline, "project-open job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn write_import_gif(path: &Path) {
        let frame = RgbaFrame::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0], 10_000).unwrap();
        let mut file = fs::File::create(path).unwrap();
        BuiltinGifEncoder::default()
            .encode_frames(vec![frame], &mut file, &EncodeOptions::default())
            .unwrap();
    }

    fn drain_import_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.import_gif_job.state() == ImportGifJobState::Running {
            app.receive_import_gif_messages();
            assert!(Instant::now() < deadline, "GIF import job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn project_root_derives_default_gif_path() {
        assert_eq!(
            default_gif_path_for_project(Path::new("/tmp/demo.gfsproj")),
            PathBuf::from("/tmp/demo.gif")
        );
        assert_eq!(
            default_gif_path_for_project(Path::new("/tmp/demo.project")),
            PathBuf::from("/tmp/demo.gif")
        );
    }

    #[test]
    fn imported_gif_derives_non_overwriting_output_and_landing_cards_fit_default_width() {
        let source = Path::new("/tmp/animation.gif");
        assert_eq!(
            edited_gif_path_for_import(source).unwrap(),
            PathBuf::from("/tmp/animation-edited.gif")
        );
        assert_ne!(edited_gif_path_for_import(source).unwrap(), source);
        assert!(landing_cards_fit(820.0 - 32.0, 8.0));
        assert!(!landing_cards_fit(550.0, 8.0));
    }

    #[test]
    fn running_open_job_locks_both_back_navigation_controls() {
        assert!(!can_navigate_back(
            AppView::OpenProject,
            OpenProjectJobState::Running,
            ImportGifJobState::Idle,
        ));
        for state in [OpenProjectJobState::Idle, OpenProjectJobState::Finished] {
            assert!(can_navigate_back(
                AppView::OpenProject,
                state,
                ImportGifJobState::Idle,
            ));
        }
        assert!(can_navigate_back(
            AppView::Editor,
            OpenProjectJobState::Running,
            ImportGifJobState::Running,
        ));
    }

    #[test]
    fn running_import_job_locks_top_and_page_back_navigation() {
        assert!(!can_navigate_back(
            AppView::ImportGif,
            OpenProjectJobState::Idle,
            ImportGifJobState::Running,
        ));
        for state in [ImportGifJobState::Idle, ImportGifJobState::Finished] {
            assert!(can_navigate_back(
                AppView::ImportGif,
                OpenProjectJobState::Idle,
                state,
            ));
        }
        assert!(can_navigate_back(
            AppView::OpenProject,
            OpenProjectJobState::Idle,
            ImportGifJobState::Running,
        ));
    }

    #[test]
    fn successful_gif_import_enters_editor_with_readable_preview_and_safe_output() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("transparent.gif");
        write_import_gif(&source);
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ImportGif;
        app.import_gif_path = source.to_string_lossy().into_owned();
        app.editor_ui_state.frame_number_input = "99".to_owned();
        app.editor_export_settings.overwrite = true;

        app.start_import_gif().unwrap();
        assert_eq!(app.import_gif_job.state(), ImportGifJobState::Running);
        drain_import_job(&mut app);

        assert_eq!(app.import_gif_job.state(), ImportGifJobState::Idle);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(
            Path::new(&app.settings.output),
            directory.path().join("transparent-edited.gif")
        );
        assert_ne!(Path::new(&app.settings.output), source);
        assert!(source.is_file());
        assert_eq!(app.editor_ui_state.frame_number_input, "1");
        assert_eq!(app.editor_export_settings, EditorExportSettings::default());
        assert_eq!(app.export_job.state(), ExportJobState::Idle);
        let (workspace_slot, preview_cache) =
            (&app.editor_workspace, &mut app.editor_preview_cache);
        let workspace = workspace_slot.as_ref().unwrap();
        assert_eq!(
            workspace.project_root(),
            directory.path().join("transparent.gfsproj")
        );
        let frame_id = workspace.selection().current().unwrap();
        let context = egui::Context::default();
        let preview = preview_cache
            .preview(workspace.active_project(), frame_id, &context, [64, 64])
            .unwrap();
        assert_eq!(preview.rendered_size, [2, 1]);
        assert!(workspace.project_root().join("project.lock").is_file());
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("will not overwrite")
        );
    }

    #[test]
    fn malformed_gif_import_resets_job_and_can_retry() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("retry.gif");
        fs::write(&source, b"broken").unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ImportGif;
        app.import_gif_path = source.to_string_lossy().into_owned();

        app.start_import_gif().unwrap();
        drain_import_job(&mut app);

        assert_eq!(app.import_gif_job.state(), ImportGifJobState::Idle);
        assert_eq!(app.view, AppView::ImportGif);
        assert!(app.editor_workspace.is_none());
        assert!(app.notice.as_deref().unwrap().contains("You can correct"));
        assert!(!source.with_extension("gfsproj").exists());

        write_import_gif(&source);
        app.start_import_gif().unwrap();
        drain_import_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert!(app.editor_workspace.is_some());
    }

    #[test]
    fn successful_open_job_enters_editor_and_resets_editor_state() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("existing.gfsproj");
        drop(single_frame_project(&root));
        let output = directory.path().join("existing.gif");
        fs::write(&output, b"existing").unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::OpenProject;
        app.open_project_path = root.to_string_lossy().into_owned();
        app.editor_ui_state.frame_number_input = "99".to_owned();
        app.editor_export_settings.overwrite = true;

        app.start_open_project().unwrap();
        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Running);
        drain_open_job(&mut app);

        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Idle);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(Path::new(&app.settings.output), output);
        assert_eq!(app.editor_ui_state.frame_number_input, "1");
        assert_eq!(app.editor_export_settings, EditorExportSettings::default());
        assert_eq!(
            app.editor_workspace.as_ref().unwrap().selection().current(),
            Some(FrameId::from_u128(7))
        );
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("Opened 1 frame"));
        assert!(notice.contains("enable Overwrite"));
    }

    #[test]
    fn failed_open_job_returns_to_retryable_open_view() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("broken.gfsproj");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("manifest.json"), b"{broken").unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::OpenProject;
        app.open_project_path = root.to_string_lossy().into_owned();

        app.start_open_project().unwrap();
        drain_open_job(&mut app);

        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Idle);
        assert_eq!(app.view, AppView::OpenProject);
        assert!(app.editor_workspace.is_none());
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Could not open project")
        );
        app.start_open_project().unwrap();
        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Running);
    }

    #[test]
    fn stopped_recording_project_switches_to_editor_and_keeps_the_lock() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("recording.gfsproj");
        let project = single_frame_project(&root);
        let mut view = AppView::ScreenRecorder;
        let mut workspace: Option<EditorWorkspace> = None;

        let summary = activate_editor(&mut view, &mut workspace, project).unwrap();

        assert_eq!(view, AppView::Editor);
        assert_eq!(summary.frames, 1);
        assert_eq!(summary.duration_us, 10_000);
        assert_eq!(summary.project_path, root);
        assert_eq!(
            workspace.as_ref().unwrap().selection().current(),
            Some(FrameId::from_u128(7))
        );
        assert!(root.join("project.lock").exists());
    }

    #[test]
    fn discarded_completed_project_is_removed_after_releasing_its_lock() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("discarded.gfsproj");
        let gif_output = directory.path().join("discarded.gif");
        let project = empty_project(&root);

        remove_completed_project(project).unwrap();

        assert!(!root.exists());
        assert!(!gif_output.exists());
    }

    #[test]
    fn preview_dimensions_preserve_landscape_and_portrait_aspect_ratios() {
        assert_eq!(fit_dimensions(3_840, 2_160, 1_600, 900), (1_600, 900));
        assert_eq!(fit_dimensions(2_160, 3_840, 1_600, 900), (506, 900));
        assert_eq!(fit_dimensions(640, 480, 1_600, 900), (640, 480));
    }

    #[test]
    fn preview_selection_maps_back_to_physical_source_pixels() {
        let preview = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(100.0, 50.0));
        let selection = egui::Rect::from_min_max(egui::pos2(30.0, 15.0), egui::pos2(80.0, 35.0));
        let mapped = map_preview_selection(preview, selection, 1_000, 500).unwrap();
        assert_eq!((mapped.origin().x, mapped.origin().y), (100, 50));
        assert_eq!((mapped.size().width(), mapped.size().height()), (500, 200));
    }

    #[test]
    fn preview_downsampling_uses_nearest_source_pixel() {
        let source = vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255];
        let resized = resize_nearest_rgba(&source, 4, 1, 2, 1).unwrap();
        assert_eq!(resized, [1, 0, 0, 255, 3, 0, 0, 255]);
    }

    #[test]
    fn recorder_stage_permissions_keep_size_locked_after_ready() {
        for stage in [
            RecorderStage::Ready,
            RecorderStage::Countdown(3),
            RecorderStage::Recording,
            RecorderStage::Paused,
        ] {
            assert!(stage.allows_moving());
        }
        assert!(!RecorderStage::Finalizing.allows_moving());
        assert!(RecorderStage::Ready.allows_resizing());
        for stage in [
            RecorderStage::Countdown(3),
            RecorderStage::Recording,
            RecorderStage::Paused,
            RecorderStage::Finalizing,
        ] {
            assert!(!stage.allows_resizing());
        }
        assert!(RecorderStage::Recording.allows_retargeting());
        assert!(RecorderStage::Paused.allows_retargeting());
        assert!(!RecorderStage::Ready.allows_retargeting());
        assert!(!RecorderStage::Countdown(3).allows_retargeting());
        assert!(!RecorderStage::Finalizing.allows_retargeting());
    }

    #[test]
    fn overlay_geometry_updates_size_only_while_ready() {
        let mut settings = RecordingSettings::default();
        apply_overlay_region(
            &mut settings,
            RecorderStage::Ready,
            gif_from_screen_capture::PhysicalRect::new(10, 20, 800, 600).unwrap(),
        );
        assert_eq!(
            (
                settings.region_x,
                settings.region_y,
                settings.region_width,
                settings.region_height
            ),
            (10, 20, 800, 600)
        );

        for (stage, x, y) in [
            (RecorderStage::Countdown(2), 30, 40),
            (RecorderStage::Recording, 50, 60),
            (RecorderStage::Paused, 70, 80),
        ] {
            apply_overlay_region(
                &mut settings,
                stage,
                gif_from_screen_capture::PhysicalRect::new(x, y, 801, 599).unwrap(),
            );
            assert_eq!((settings.region_x, settings.region_y), (x, y));
            assert_eq!((settings.region_width, settings.region_height), (800, 600));
        }

        apply_overlay_region(
            &mut settings,
            RecorderStage::Finalizing,
            gif_from_screen_capture::PhysicalRect::new(90, 100, 800, 600).unwrap(),
        );
        assert_eq!((settings.region_x, settings.region_y), (70, 80));
        assert_eq!((settings.region_width, settings.region_height), (800, 600));
    }

    #[test]
    fn terminal_actions_and_non_recording_stages_never_schedule_retargeting() {
        assert!(should_sync_retarget(
            RecorderStage::Recording,
            RecorderOverlayAction::None
        ));
        assert!(should_sync_retarget(
            RecorderStage::Paused,
            RecorderOverlayAction::Resume
        ));
        for action in [
            RecorderOverlayAction::Stop,
            RecorderOverlayAction::Discard,
            RecorderOverlayAction::Close,
        ] {
            assert!(!should_sync_retarget(RecorderStage::Recording, action));
            assert!(!should_sync_retarget(RecorderStage::Paused, action));
        }
        for stage in [
            RecorderStage::Ready,
            RecorderStage::Countdown(1),
            RecorderStage::Finalizing,
        ] {
            assert!(!should_sync_retarget(stage, RecorderOverlayAction::None));
        }
    }
}
