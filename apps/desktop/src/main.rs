#![forbid(unsafe_code)]

//! Desktop entry point for the Linux-first `GifFromScreen` application.

mod appearance;
mod blank_project_job;
mod blank_project_ui;
mod capture_source_job;
mod countdown;
mod custom_palette_input;
mod editor_preview;
mod editor_ui;
mod editor_workspace;
mod export_job;
mod fixed_crop_session;
mod import_gif_job;
mod import_static_image_job;
mod import_static_sequence_job;
mod open_project_job;
mod retarget;
mod static_sequence_ui;
mod text_overlay_ui;
mod thumbnail_cache;
mod watermark_decode_job;
mod watermark_ui;
mod wayland_prepare_job;

use std::{
    collections::{BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    fs, io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use blank_project_job::{
    BlankProjectJob, BlankProjectJobEvent, BlankProjectJobState, BlankProjectRequest,
};
use blank_project_ui::{
    BlankBackgroundChoice, BlankProjectUiAction, BlankProjectUiState, show_blank_project_ui,
};
use capture_source_job::{CaptureSourceJob, CaptureSourceJobState};
use countdown::{CountdownStart, CountdownTick, MAX_COUNTDOWN_SECONDS, RecordingCountdown};
use custom_palette_input::parse_custom_palette;
use editor_preview::EditorPreviewCache;
use editor_ui::{
    DrawingDraftPhase, DrawingOverlayDraft, EditorUiAction, EditorUiResult, EditorUiState,
    OverlayTool, show_editor_chrome, show_editor_tool_panel,
};
use editor_workspace::EditorWorkspace;
use eframe::egui;
use export_job::{ExportJob, ExportJobError, ExportJobEvent, ExportJobState};
use gif_from_screen_application::{
    DEFAULT_BLANK_FRAME_LIMIT_BYTES, IncrementalRecordingProject,
    IncrementalRecordingProjectOptions, ProjectExportSnapshot, ProjectFrameSelection,
    ProjectGifExportOptions, ProjectGifExportReport,
};
use gif_from_screen_capture::{
    CaptureCadence, CaptureRequest, CaptureSource, CaptureSourceId, CaptureSourceKind,
    CaptureTarget, CapturedFrame, CursorCaptureMode, PhysicalRect, PixelFormat,
};
use gif_from_screen_capture_linux::{LinuxDisplayServer, X11CaptureBackend};
use gif_from_screen_domain::{
    DurationUs, FrameId, PhysicalPoint as ProjectPhysicalPoint, PhysicalPx,
    PhysicalSize as ProjectPhysicalSize, ProjectId, Rgba, StrokePoint, UnixTimeMs,
};
use gif_from_screen_gif::{
    CancellationFlag, CancellationToken as _, DeltaMode, DitherMode, EncodeOptions, LoopBehavior,
    PaletteMode, QuantizerStrategy, Transparency,
};
use gif_from_screen_media::{
    DecodeLimits, LoopBehavior as ImportedLoopBehavior, StaticImageSequenceDurationPolicy,
};
use gif_from_screen_project::{ActiveProject, LockPolicy, OpenedProject, ProjectError};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, FrameRetention, RecordingControl, RecordingController,
    RecordingFrameSink, RecordingFrameSinkError, SnapshotTriggerRequest, SnapshotTriggerStatus,
    TargetUpdateRequest, TargetUpdateStatus, WorkflowError, WorkflowProgress,
    collect_controlled_to_sink, collect_prestarted_controlled_to_sink,
};
use import_gif_job::{ImportGifJob, ImportGifJobEvent, ImportGifJobState};
use import_static_image_job::{
    ImportStaticImageJob, ImportStaticImageJobEvent, ImportStaticImageJobState,
};
use import_static_sequence_job::{
    ImportStaticSequenceJob, ImportStaticSequenceJobEvent, ImportStaticSequenceJobState,
    ImportStaticSequenceRequest,
};
use open_project_job::{
    OpenProjectJob, OpenProjectJobError, OpenProjectJobEvent, OpenProjectJobState,
};
use retarget::{RegionRetargetPlan, RetargetCompletion};
use static_sequence_ui::{
    StaticSequenceLoopChoice, StaticSequenceTimingChoice, StaticSequenceUiAction,
    StaticSequenceUiState, show_static_sequence_ui,
};
use text_overlay_ui::TextOverlayTool;
use uuid::Uuid;
use watermark_decode_job::{WatermarkDecodeEvent, WatermarkDecodeJob, WatermarkDecodeJobState};
use watermark_ui::{PendingWatermark, WatermarkUiAction, WatermarkUiState, show_watermark_ui};
use wayland_prepare_job::{
    FrozenSourcePreview, WaylandPrepareJob, WaylandPrepareJobEvent, WaylandPrepareJobState,
    WaylandPrepareOutcome,
};

const APP_NAME: &str = "GifFromScreen";
const RECORDER_BORDER_POINTS: f32 = 4.0;
const RECORDER_TOOLBAR_POINTS: f32 = 76.0;
const MAX_RECORDING_DURATION_MS: u64 = 3_600_000;
const EDITOR_HISTORY_LIMIT: usize = 100;
const EDITOR_PREVIEW_MAX_SIZE: [u32; 2] = [960, 540];
const LANDING_COLUMN_COUNT: usize = 2;
const LANDING_CARD_MIN_WIDTH: f32 = 280.0;
const MAX_STATIC_SEQUENCE_FRAMES: usize = 10_000;
const MAX_STATIC_SEQUENCE_EDGE: u16 = 16_384;
const MAX_STATIC_SEQUENCE_RGBA_BYTES: u64 = 512 * 1024 * 1024;

fn recorder_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("gif-from-screen-recorder-frame")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AppView {
    #[default]
    Landing,
    OpenProject,
    ImportGif,
    ImportImage,
    ImportImageSequence,
    NewBlankAnimation,
    ScreenRecorder,
    Editor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StartupIntent {
    None,
    OpenProject(PathBuf),
    ImportGif(PathBuf),
    ImportImage(PathBuf),
    Invalid(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileDropRoute {
    OpenProject(PathBuf),
    ImportGif(PathBuf),
    ImportImage(PathBuf),
    ImportImageSequence(Vec<PathBuf>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileDropCandidate {
    path: PathBuf,
    is_directory: bool,
    is_regular_file: bool,
    has_project_manifest: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FileDropActivity {
    #[default]
    Idle,
    Recording,
    ProjectOpen,
    GifImport,
    ImageImport,
    ImageSequenceImport,
    BlankCreation,
    WatermarkDecode,
    Export,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RecordingCadenceChoice {
    #[default]
    FixedFps,
    Periodic,
    Manual,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RecordingIntervalUnit {
    #[default]
    Seconds,
    Minutes,
    Hours,
}

#[derive(Clone, Debug)]
struct RecordingSettings {
    output: String,
    duration_ms: u64,
    cadence: RecordingCadenceChoice,
    fps: u32,
    interval_count: u32,
    interval_unit: RecordingIntervalUnit,
    manual_frame_duration_ms: u64,
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
            cadence: RecordingCadenceChoice::FixedFps,
            fps: 10,
            interval_count: 1,
            interval_unit: RecordingIntervalUnit::Seconds,
            manual_frame_duration_ms: 100,
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
    Finished(RecordingCompletion),
}

enum RecordingCompletion {
    Completed(Box<ActiveProject>),
    Discarded {
        cleanup_error: Option<String>,
    },
    Failed {
        error: String,
        recovery_path: Option<PathBuf>,
    },
}

struct RecordingWorkerRequest {
    settings: RecordingSettings,
    source_id: CaptureSourceId,
    source_kind: CaptureSourceKind,
    source_label: String,
    project_path: PathBuf,
    canvas: ProjectPhysicalSize,
}

struct IncrementalProjectFrameSink {
    project: IncrementalRecordingProject,
    frame_ids: Vec<FrameId>,
}

impl IncrementalProjectFrameSink {
    fn new(project: IncrementalRecordingProject) -> Self {
        Self {
            project,
            frame_ids: Vec::new(),
        }
    }

    fn root(&self) -> &Path {
        self.project.root()
    }

    fn finish(
        self,
    ) -> Result<ActiveProject, gif_from_screen_application::IncrementalRecordingProjectError> {
        self.project.finish()
    }
}

impl RecordingFrameSink for IncrementalProjectFrameSink {
    fn append_provisional_frame(
        &mut self,
        frame_index: u64,
        frame: &gif_from_screen_gif::RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        let expected = u64::try_from(self.frame_ids.len()).unwrap_or(u64::MAX);
        if frame_index != expected {
            return Err(io::Error::other(format!(
                "incremental frame index {frame_index} does not follow {expected}"
            ))
            .into());
        }
        let frame_id = FrameId::from_u128(Uuid::new_v4().as_u128());
        self.project.append_frame(frame_id, frame)?;
        self.frame_ids.push(frame_id);
        Ok(())
    }

    fn update_frame_duration(
        &mut self,
        frame_index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        let index = usize::try_from(frame_index)
            .map_err(|_| io::Error::other("incremental frame index exceeds usize"))?;
        let frame_id = self.frame_ids.get(index).copied().ok_or_else(|| {
            io::Error::other(format!("incremental frame {frame_index} was not appended"))
        })?;
        let duration = DurationUs::new(duration_us)
            .ok_or_else(|| io::Error::other("incremental frame duration must be positive"))?;
        self.project.set_frame_duration(frame_id, duration)?;
        Ok(())
    }
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
    Wu,
    Grayscale,
    MostUsed,
    NeuQuant,
    WebSafe216,
    Monochrome,
    Windows16,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExportDitherChoice {
    #[default]
    None,
    Bayer,
    Dotted,
    BlueNoise,
    InterleavedNoise,
    FloydSteinberg,
    Atkinson,
    Burkes,
    Sierra,
    SierraLite,
    TwoRowSierra,
    JarvisJudiceNinke,
    Stucki,
    StevensonArce,
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
    custom_palette_text: String,
    custom_transparency_enabled: bool,
    custom_transparent_index: u16,
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
            custom_palette_text: "#000000\n#FFFFFF".to_owned(),
            custom_transparency_enabled: false,
            custom_transparent_index: 0,
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
    snapshot_requests: VecDeque<SnapshotTriggerRequest>,
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

    fn trigger_snapshot(&mut self) {
        if !self.terminal_requested {
            self.snapshot_requests
                .push_back(self.controller.trigger_snapshot());
        }
    }

    fn poll_snapshots(&mut self) -> Option<String> {
        let request_count = self.snapshot_requests.len();
        let mut notice = None;
        for _ in 0..request_count {
            let Some(mut request) = self.snapshot_requests.pop_front() else {
                break;
            };
            match request.status() {
                SnapshotTriggerStatus::Pending => self.snapshot_requests.push_back(request),
                SnapshotTriggerStatus::Captured(receipt) => {
                    notice = Some(format!(
                        "Snapshot captured from native frame {} at {:.3}s.",
                        receipt.sequence(),
                        Duration::from_micros(receipt.captured_at().as_micros()).as_secs_f64()
                    ));
                }
                SnapshotTriggerStatus::Rejected(reason) => {
                    notice = Some(format!("Snapshot was not captured: {reason}"));
                }
                SnapshotTriggerStatus::WorkerExited => {
                    notice = Some(
                        "Snapshot was not captured because the recording worker exited.".to_owned(),
                    );
                }
                _ => {
                    self.snapshot_requests.push_back(request);
                }
            }
        }
        notice
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

struct WaylandFrozenPreview {
    texture: egui::TextureHandle,
    source_size: gif_from_screen_capture::PhysicalSize,
    selection: PhysicalRect,
    drag_start: Option<egui::Pos2>,
    drag_current: Option<egui::Pos2>,
    drag_initial_region: Option<PhysicalRect>,
}

struct WaylandCropController {
    texture: egui::TextureHandle,
    source_size: gif_from_screen_capture::PhysicalSize,
    region: PhysicalRect,
    drag_start: Option<egui::Pos2>,
    drag_current: Option<egui::Pos2>,
    drag_initial_region: Option<PhysicalRect>,
}

struct RecorderOverlay {
    initial_position: egui::Pos2,
    initial_size: egui::Vec2,
    initialized: bool,
    source_geometry: PhysicalRect,
}

#[derive(Clone, Copy, Debug)]
struct MainWindowSnapshot {
    position: Option<egui::Pos2>,
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
    Snapshot,
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
    display_server: Option<LinuxDisplayServer>,
    source_catalog_attempted: bool,
    source_catalog_job: CaptureSourceJob,
    wayland_prepare_job: WaylandPrepareJob,
    wayland_frozen_preview: Option<WaylandFrozenPreview>,
    wayland_crop_controller: Option<WaylandCropController>,
    region_picker: Option<RegionPicker>,
    recorder_overlay: Option<RecorderOverlay>,
    main_window_snapshot: Option<MainWindowSnapshot>,
    restore_main_window: bool,
    recording_countdown: RecordingCountdown,
    job: Option<RecordingJob>,
    progress: Option<WorkflowProgress>,
    open_project_path: String,
    open_project_take_over_lock: bool,
    open_project_job: OpenProjectJob,
    import_gif_path: String,
    import_gif_job: ImportGifJob,
    import_image_path: String,
    import_image_job: ImportStaticImageJob,
    import_sequence_ui: StaticSequenceUiState,
    import_sequence_job: ImportStaticSequenceJob,
    blank_project_ui: BlankProjectUiState,
    blank_project_job: BlankProjectJob,
    watermark_ui: WatermarkUiState,
    watermark_job: WatermarkDecodeJob,
    text_overlay: TextOverlayTool,
    pending_watermark: Option<PendingWatermark>,
    editor_workspace: Option<EditorWorkspace>,
    editor_ui_state: EditorUiState,
    editor_preview_cache: EditorPreviewCache,
    editor_export_settings: EditorExportSettings,
    export_job: ExportJob,
}

impl Default for GifFromScreenApp {
    fn default() -> Self {
        Self {
            view: AppView::Landing,
            notice: None,
            settings: RecordingSettings::default(),
            sources: Vec::new(),
            selected_source: 0,
            display_server: None,
            source_catalog_attempted: false,
            source_catalog_job: CaptureSourceJob::default(),
            wayland_prepare_job: WaylandPrepareJob::default(),
            wayland_frozen_preview: None,
            wayland_crop_controller: None,
            region_picker: None,
            recorder_overlay: None,
            main_window_snapshot: None,
            restore_main_window: false,
            recording_countdown: RecordingCountdown::default(),
            job: None,
            progress: None,
            open_project_path: String::new(),
            open_project_take_over_lock: false,
            open_project_job: OpenProjectJob::default(),
            import_gif_path: String::new(),
            import_gif_job: ImportGifJob::default(),
            import_image_path: String::new(),
            import_image_job: ImportStaticImageJob::default(),
            import_sequence_ui: StaticSequenceUiState::default(),
            import_sequence_job: ImportStaticSequenceJob::default(),
            blank_project_ui: BlankProjectUiState::default(),
            blank_project_job: BlankProjectJob::default(),
            watermark_ui: WatermarkUiState::default(),
            watermark_job: WatermarkDecodeJob::default(),
            text_overlay: TextOverlayTool::default(),
            pending_watermark: None,
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
        self.receive_capture_source_result();
        self.receive_wayland_prepare_messages(context);
        self.receive_job_messages();
        self.receive_export_messages();
        self.receive_open_project_messages();
        self.receive_import_gif_messages();
        self.receive_import_image_messages();
        self.receive_import_sequence_messages();
        self.receive_blank_project_messages();
        self.receive_watermark_messages();
        self.receive_text_messages();
        let dropped_paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        });
        if !dropped_paths.is_empty() {
            self.handle_dropped_paths(&dropped_paths);
        }
        self.advance_recording_countdown(context);
        if self.restore_main_window {
            if let Some(snapshot) = self.main_window_snapshot.take() {
                context.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                context.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                    680.0, 440.0,
                )));
                context.send_viewport_cmd(egui::ViewportCommand::InnerSize(snapshot.size));
                if let Some(position) = snapshot.position {
                    context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                }
            }
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.restore_main_window = false;
        }
        if self.recorder_overlay.is_some() {
            self.show_recorder_overlay(context);
        }
        if self.wayland_crop_controller.is_some() {
            self.show_wayland_crop_controller(context);
        }
        if self.job.is_some()
            || self.recorder_overlay.is_some()
            || self.wayland_crop_controller.is_some()
            || self.recording_countdown.is_active()
            || export_job_is_active(self.export_job.state())
            || self.open_project_job.state() == OpenProjectJobState::Running
            || self.import_gif_job.state() == ImportGifJobState::Running
            || self.import_image_job.state() == ImportStaticImageJobState::Running
            || self.import_sequence_job.state() == ImportStaticSequenceJobState::Running
            || self.blank_project_job.state() == BlankProjectJobState::Running
            || self.watermark_job.state() == WatermarkDecodeJobState::Running
            || self.text_overlay.is_running()
            || self.source_catalog_job.state() == CaptureSourceJobState::Loading
            || self.wayland_prepare_job.is_active()
        {
            context.request_repaint_after(Duration::from_millis(33));
        }

        egui::TopBottomPanel::top("app_header").show(context, |ui| {
            ui.horizontal(|ui| {
                let back_enabled = self.watermark_job.state() != WatermarkDecodeJobState::Running
                    && can_navigate_back(
                        self.view,
                        self.open_project_job.state(),
                        self.import_gif_job.state(),
                        self.import_image_job.state(),
                        self.import_sequence_job.state(),
                        self.blank_project_job.state(),
                    );
                if self.view != AppView::Landing
                    && ui
                        .add_enabled(back_enabled, egui::Button::new("Back"))
                        .clicked()
                {
                    if self.view == AppView::ScreenRecorder && self.wayland_prepare_job.is_active()
                    {
                        let _ = self.wayland_prepare_job.cancel();
                        self.wayland_frozen_preview = None;
                    }
                    self.view = AppView::Landing;
                }
                ui.heading(APP_NAME);
                ui.separator();
                ui.label("Linux capture preview");
            });
        });

        egui::CentralPanel::default().show(context, |ui| match self.view {
            AppView::Landing => self.show_landing(ui),
            AppView::OpenProject => self.show_open_project(ui),
            AppView::ImportGif => self.show_import_gif(ui),
            AppView::ImportImage => self.show_import_image(ui),
            AppView::ImportImageSequence => self.show_import_sequence(ui),
            AppView::NewBlankAnimation => self.show_blank_project(ui),
            AppView::ScreenRecorder => self.show_screen_recorder(ui),
            AppView::Editor => self.show_editor(ui),
        });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

impl GifFromScreenApp {
    fn handle_dropped_paths(&mut self, dropped_paths: &[Option<PathBuf>]) {
        if let Some(reason) = file_drop_block_reason(self.file_drop_activity()) {
            self.notice = Some(format!(
                "Cannot accept dropped files while {reason}. Finish the active operation and try again."
            ));
            return;
        }
        let route = match prepare_file_drop_route(dropped_paths) {
            Ok(route) => route,
            Err(error) => {
                self.notice = Some(error);
                return;
            }
        };
        if let Err(error) = self.start_file_drop_route(route) {
            self.notice = Some(format!("Could not start dropped-file operation: {error}"));
        }
    }

    fn file_drop_activity(&self) -> FileDropActivity {
        if self.job.is_some()
            || self.recorder_overlay.is_some()
            || self.recording_countdown.is_active()
            || self.region_picker.is_some()
            || self.wayland_prepare_job.is_active()
            || self.wayland_frozen_preview.is_some()
            || self.wayland_crop_controller.is_some()
        {
            FileDropActivity::Recording
        } else if self.open_project_job.state() != OpenProjectJobState::Idle {
            FileDropActivity::ProjectOpen
        } else if self.import_gif_job.state() != ImportGifJobState::Idle {
            FileDropActivity::GifImport
        } else if self.import_image_job.state() != ImportStaticImageJobState::Idle {
            FileDropActivity::ImageImport
        } else if self.import_sequence_job.state() != ImportStaticSequenceJobState::Idle {
            FileDropActivity::ImageSequenceImport
        } else if self.blank_project_job.state() != BlankProjectJobState::Idle {
            FileDropActivity::BlankCreation
        } else if self.watermark_job.state() != WatermarkDecodeJobState::Idle {
            FileDropActivity::WatermarkDecode
        } else if self.export_job.state() != ExportJobState::Idle {
            FileDropActivity::Export
        } else {
            FileDropActivity::Idle
        }
    }

    fn start_file_drop_route(&mut self, route: FileDropRoute) -> Result<(), String> {
        match route {
            FileDropRoute::OpenProject(path) => {
                self.view = AppView::OpenProject;
                self.open_project_path = path.to_string_lossy().into_owned();
                self.open_project_take_over_lock = false;
                self.start_open_project()
            }
            FileDropRoute::ImportGif(path) => {
                self.view = AppView::ImportGif;
                self.import_gif_path = path.to_string_lossy().into_owned();
                self.start_import_gif()
            }
            FileDropRoute::ImportImage(path) => {
                self.view = AppView::ImportImage;
                self.import_image_path = path.to_string_lossy().into_owned();
                self.start_import_image()
            }
            FileDropRoute::ImportImageSequence(inputs) => {
                let target = default_sequence_project_path(&inputs)?;
                self.import_sequence_ui.replace_inputs(inputs, &target);
                self.view = AppView::ImportImageSequence;
                self.notice = Some(
                    "Image sequence loaded in dropped-file order. Review timing, loop, and target, then start the bounded import."
                        .to_owned(),
                );
                Ok(())
            }
        }
    }

    fn apply_startup_intent(&mut self, intent: StartupIntent) {
        match intent {
            StartupIntent::None => {}
            StartupIntent::OpenProject(path) => {
                self.view = AppView::OpenProject;
                self.open_project_path = path.to_string_lossy().into_owned();
                self.open_project_take_over_lock = false;
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
            StartupIntent::ImportImage(path) => {
                self.view = AppView::ImportImage;
                self.import_image_path = path.to_string_lossy().into_owned();
                if let Err(error) = self.start_import_image() {
                    self.notice = Some(format!("Could not import startup image: {error}"));
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
                    "Record a Linux monitor, window, or physical-pixel region.",
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
                    self.open_project_take_over_lock = false;
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
                if landing_action(
                    &mut columns[1],
                    "Import image",
                    "Import PNG, JPEG, BMP, or WebP as a one-frame project.",
                    true,
                ) {
                    self.view = AppView::ImportImage;
                    self.notice = Some(
                        "Static image import uses strict 16K and 512 MiB limits with a 100 ms frame. Import cannot currently be cancelled once started."
                            .to_owned(),
                    );
                }
            });

            ui.add_space(12.0);
            ui.columns(LANDING_COLUMN_COUNT, |columns| {
                if landing_action(
                    &mut columns[0],
                    "New blank animation",
                    "Start with a transparent or solid-color canvas.",
                    true,
                ) {
                    self.blank_project_ui = BlankProjectUiState::default();
                    self.view = AppView::NewBlankAnimation;
                    self.notice = Some(
                        "Choose the canvas, background, first-frame duration, and a new project path."
                            .to_owned(),
                    );
                }
                if landing_action(
                    &mut columns[1],
                    "Import image sequence",
                    "Build an animation from ordered PNG, JPEG, BMP, or WebP files.",
                    true,
                ) {
                    self.view = AppView::ImportImageSequence;
                    self.notice = Some(
                        "Add at least two same-sized images or drop them together. Their order can be adjusted before import."
                            .to_owned(),
                    );
                }
            });

            if let Some(notice) = &self.notice {
                ui.add_space(24.0);
                ui.label(notice);
            }
        });
    }

    fn show_open_project(&mut self, ui: &mut egui::Ui) {
        let running = self.open_project_job.state() == OpenProjectJobState::Running;
        let controls_enabled = open_project_controls_enabled(self.open_project_job.state());
        ui.heading("Open editable project");
        ui.label("Choose an existing .gfsproj directory containing manifest.json.");
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("Project directory");
            ui.add_enabled(
                controls_enabled,
                egui::TextEdit::singleline(&mut self.open_project_path)
                    .desired_width(420.0)
                    .hint_text("/path/to/animation.gfsproj"),
            );
        });
        ui.add_space(8.0);
        ui.add_enabled_ui(controls_enabled, |ui| {
            ui.checkbox(
                &mut self.open_project_take_over_lock,
                "I confirm the previous project owner has stopped; preserve and take over its lock",
            );
        });
        if self.open_project_take_over_lock {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Warning: takeover can corrupt the project if another process is still editing it. The old lock will be preserved as project.lock.stale-N.",
            );
        } else {
            ui.weak(
                "Safe default: an existing lock is rejected unchanged. Only take over after verifying the owner stopped; takeover while another process edits can corrupt the project.",
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let open_label = if self.open_project_take_over_lock {
                "Take over lock and open"
            } else {
                "Open"
            };
            if ui
                .add_enabled(controls_enabled, egui::Button::new(open_label))
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
                        self.import_image_job.state(),
                        self.import_sequence_job.state(),
                        self.blank_project_job.state(),
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
            .start(
                PathBuf::from(path),
                open_project_lock_policy(self.open_project_take_over_lock),
            )
            .map_err(|error| error.to_string())?;
        self.notice = Some(if self.open_project_take_over_lock {
            "Opening project with explicit stale-lock takeover…".to_owned()
        } else {
            "Opening project in the background…".to_owned()
        });
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
                        self.import_image_job.state(),
                        self.import_sequence_job.state(),
                        self.blank_project_job.state(),
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

    fn show_import_image(&mut self, ui: &mut egui::Ui) {
        let running = self.import_image_job.state() == ImportStaticImageJobState::Running;
        ui.heading("Import static image as editable project");
        ui.label(
            "Choose a regular PNG, JPEG, BMP, or WebP file. A sibling <stem>.gfsproj will be created.",
        );
        ui.weak(
            "Safety limits: 16K dimensions and 512 MiB decoded RGBA. The single frame lasts 100 ms. This operation cannot currently be cancelled.",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("Image file");
            ui.add_enabled(
                !running,
                egui::TextEdit::singleline(&mut self.import_image_path)
                    .desired_width(420.0)
                    .hint_text("/path/to/image.png"),
            );
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Import"))
                .clicked()
                && let Err(error) = self.start_import_image()
            {
                self.notice = Some(format!("Could not start image import: {error}"));
            }
            if ui
                .add_enabled(
                    can_navigate_back(
                        self.view,
                        self.open_project_job.state(),
                        self.import_gif_job.state(),
                        self.import_image_job.state(),
                        self.import_sequence_job.state(),
                        self.blank_project_job.state(),
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

    fn start_import_image(&mut self) -> Result<(), String> {
        let path = self.import_image_path.trim();
        if path.is_empty() {
            return Err("Select a PNG, JPEG, BMP, or WebP file first.".to_owned());
        }
        self.import_image_job
            .start(PathBuf::from(path))
            .map_err(|error| error.to_string())?;
        self.notice = Some(
            "Importing static image in the background. The bounded decode/persist operation cannot be cancelled."
                .to_owned(),
        );
        Ok(())
    }

    fn show_import_sequence(&mut self, ui: &mut egui::Ui) {
        let running = self.import_sequence_job.state() == ImportStaticSequenceJobState::Running;
        match show_static_sequence_ui(ui, &mut self.import_sequence_ui, running) {
            StaticSequenceUiAction::None => {}
            StaticSequenceUiAction::Start => {
                if let Err(error) = self.start_import_sequence() {
                    self.notice = Some(format!("Could not start image-sequence import: {error}"));
                }
            }
            StaticSequenceUiAction::Back => self.view = AppView::Landing,
            StaticSequenceUiAction::Notice(message) => self.notice = Some(message),
        }
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn start_import_sequence(&mut self) -> Result<(), String> {
        let request = build_static_sequence_request(&self.import_sequence_ui)?;
        let frame_count = request.inputs.len();
        self.import_sequence_job
            .start(request)
            .map_err(|error| error.to_string())?;
        self.notice = Some(format!(
            "Importing {frame_count} ordered images in the background. This bounded operation cannot be cancelled."
        ));
        Ok(())
    }

    fn show_blank_project(&mut self, ui: &mut egui::Ui) {
        let running = self.blank_project_job.state() == BlankProjectJobState::Running;
        match show_blank_project_ui(ui, &mut self.blank_project_ui, running) {
            BlankProjectUiAction::None => {}
            BlankProjectUiAction::Start => {
                if let Err(error) = self.start_blank_project() {
                    self.notice = Some(format!("Could not start blank project creation: {error}"));
                }
            }
            BlankProjectUiAction::Back => self.view = AppView::Landing,
        }
        if let Some(notice) = &self.notice {
            ui.add_space(12.0);
            ui.label(notice);
        }
    }

    fn start_blank_project(&mut self) -> Result<(), String> {
        let request = build_blank_project_request(&self.blank_project_ui)?;
        self.blank_project_job
            .start(request)
            .map_err(|error| error.to_string())?;
        self.notice = Some(
            "Creating the bounded blank animation in the background. This operation cannot be cancelled."
                .to_owned(),
        );
        Ok(())
    }

    fn show_screen_recorder(&mut self, ui: &mut egui::Ui) {
        self.ensure_capture_source_catalog();
        if self.wayland_prepare_job.is_active() || self.wayland_frozen_preview.is_some() {
            self.show_wayland_preparation(ui);
            return;
        }
        if self.region_picker.is_some() {
            self.show_region_picker(ui);
            return;
        }
        ui.heading(match self.display_server {
            Some(LinuxDisplayServer::Wayland) => "Wayland screen recorder",
            Some(LinuxDisplayServer::X11) => "X11 screen recorder",
            Some(_) | None => "Linux screen recorder",
        });
        ui.label("Capture and durable project creation run on a background worker.");
        ui.add_space(12.0);
        self.show_recording_settings(ui);

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.job.is_none()
                        && self.recorder_overlay.is_none()
                        && !self.sources.is_empty()
                        && self.source_catalog_job.state() != CaptureSourceJobState::Loading,
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
        self.show_editor_work_area(ui);
        let Some(workspace) = &self.editor_workspace else {
            return;
        };
        ui.separator();
        let export_action = egui::CollapsingHeader::new("Export GIF")
            .id_salt("editor-export-options")
            .show(ui, |ui| {
                show_export_panel(
                    ui,
                    &mut self.settings.output,
                    &mut self.editor_export_settings,
                    &self.export_job,
                    workspace.selection().len(),
                    workspace.asset_issues().len(),
                    self.watermark_job.state() == WatermarkDecodeJobState::Running,
                )
            })
            .body_returned
            .unwrap_or(EditorExportAction::None);
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

    fn show_editor_work_area(&mut self, ui: &mut egui::Ui) {
        let Some(workspace) = &mut self.editor_workspace else {
            ui.label("No active editor project.");
            return;
        };
        let watermark_running = self.watermark_job.state() == WatermarkDecodeJobState::Running;
        let mut results = ui
            .add_enabled_ui(!watermark_running, |ui| {
                show_editor_chrome(ui, workspace, &mut self.editor_ui_state)
            })
            .inner;
        let mut watermark_action = WatermarkUiAction::None;
        let mut inspector =
            |ui: &mut egui::Ui, workspace: &mut EditorWorkspace, state: &mut EditorUiState| {
                let (tool_results, notice, action) = show_editor_inspector(
                    ui,
                    workspace,
                    state,
                    &mut self.text_overlay,
                    &mut self.watermark_ui,
                    self.watermark_job.state(),
                );
                results.extend(tool_results);
                if notice.is_some() {
                    self.notice = notice;
                }
                watermark_action = action;
            };
        if ui.available_width() >= 900.0 {
            ui.columns(2, |columns| {
                inspector(&mut columns[0], workspace, &mut self.editor_ui_state);
                show_editor_preview_panel(
                    &mut columns[1],
                    workspace,
                    &mut self.editor_preview_cache,
                    &mut self.editor_ui_state,
                );
            });
        } else {
            show_editor_preview_panel(
                ui,
                workspace,
                &mut self.editor_preview_cache,
                &mut self.editor_ui_state,
            );
            ui.separator();
            inspector(ui, workspace, &mut self.editor_ui_state);
        }
        for result in results {
            if let Some(notice) = editor_result_notice(result) {
                self.notice = Some(notice);
            }
        }
        if watermark_action == WatermarkUiAction::Start {
            match PendingWatermark::start(&self.watermark_ui, workspace, &mut self.watermark_job) {
                Ok(pending) => {
                    self.pending_watermark = Some(pending);
                    self.notice = Some("Decoding watermark in the background…".to_owned());
                }
                Err(error) => self.notice = Some(format!("Could not decode watermark: {error}")),
            }
        }
    }

    fn show_recording_settings(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("recording_settings")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Capture source");
                ui.horizontal(|ui| {
                    let selected_name = self.sources.get(self.selected_source).map_or_else(
                        || "No capture source".to_owned(),
                        |source| source.name().into(),
                    );
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
                    if self.source_catalog_job.state() == CaptureSourceJobState::Loading {
                        ui.spinner();
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
                show_recording_cadence_settings(ui, &mut self.settings);
                show_frame_retention_setting(ui, &mut self.settings);
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
        } else if self.display_server == Some(LinuxDisplayServer::Wayland)
            && self.sources.get(self.selected_source).is_some()
        {
            ui.weak(
                "Wayland keeps source geometry private. The system chooser will open in the background, then the first PipeWire frame will provide a frozen preview.",
            );
        }
    }

    fn begin_region_picker(&mut self, context: &egui::Context) -> Result<(), String> {
        if self.display_server == Some(LinuxDisplayServer::Wayland) {
            return self.begin_wayland_preparation();
        }
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
        if self.display_server == Some(LinuxDisplayServer::Wayland) {
            return self.begin_wayland_preparation();
        }
        let main_window = context
            .input(|input| {
                let viewport = input.viewport();
                Some(MainWindowSnapshot {
                    position: Some(viewport.outer_rect?.min),
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
        if let Some(snapshot_notice) = self.job.as_mut().and_then(RecordingJob::poll_snapshots) {
            self.notice = Some(snapshot_notice);
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
        let manual_snapshots = self.settings.cadence == RecordingCadenceChoice::Manual;
        let frame = context.show_viewport_immediate(
            recorder_viewport_id(),
            builder,
            |viewport_context, _class| {
                draw_recorder_overlay(
                    viewport_context,
                    stage,
                    progress,
                    source_geometry,
                    manual_snapshots,
                )
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
            RecorderOverlayAction::Snapshot => {
                if let Some(job) = &mut self.job {
                    job.trigger_snapshot();
                    self.notice = Some("Manual snapshot requested…".to_owned());
                    context.request_repaint();
                }
            }
            RecorderOverlayAction::Stop => {
                if let Some(job) = &mut self.job {
                    job.stop_retargeting();
                    let _ = job.controller.stop();
                    self.notice = Some("Stopping and finalizing recoverable project…".into());
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
                if (self.recorder_overlay.is_some() || self.wayland_crop_controller.is_some())
                    && self.job.is_none()
                    && let Err(error) = self.start_recording()
                {
                    self.notice = Some(error);
                }
            }
        }
    }

    fn start_recording(&mut self) -> Result<(), String> {
        if self.wayland_crop_controller.is_some() {
            return self.start_wayland_recording();
        }
        validate_settings(&self.settings)?;
        let selected = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No X11 capture source is selected.".to_owned())?;
        let settings = self.settings.clone();
        let source_id = selected.id().clone();
        let source_kind = selected.kind();
        let project_path = project_path_for_output(Path::new(settings.output.trim()))?;
        let canvas = recording_project_canvas(&settings, selected.geometry())?;
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
            canvas,
        };

        std::thread::Builder::new()
            .name("gfs-x11-record".into())
            .spawn(move || {
                let progress_sender = sender.clone();
                let mut progress = move |snapshot| {
                    let _ = progress_sender.send(JobMessage::Progress(snapshot));
                };
                let completion = run_incremental_x11_recording(
                    &worker_request,
                    &mut control,
                    &worker_cancellation,
                    &mut progress,
                    || {
                        let _ = sender.send(JobMessage::Persisting);
                    },
                );
                if let Err(error) = sender.send(JobMessage::Finished(completion))
                    && worker_cancellation.is_cancelled()
                    && let JobMessage::Finished(RecordingCompletion::Completed(project)) = error.0
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
            snapshot_requests: VecDeque::new(),
        });
        Ok(())
    }

    fn start_wayland_recording(&mut self) -> Result<(), String> {
        validate_settings(&self.settings)?;
        if self.job.is_some() {
            return Ok(());
        }
        let controller = self
            .wayland_crop_controller
            .as_ref()
            .ok_or_else(|| "Wayland source-local controller is not open.".to_owned())?;
        let region = controller.region;
        if !region.fits_within(controller.source_size) {
            return Err("Wayland crop is outside the prepared source.".to_owned());
        }
        let selected = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No Wayland portal source is selected.".to_owned())?;
        let mut settings = self.settings.clone();
        apply_wayland_region_to_settings(&mut settings, region);
        let worker = RecordingWorkerRequest {
            project_path: project_path_for_output(Path::new(settings.output.trim()))?,
            canvas: ProjectPhysicalSize::new(region.size().width(), region.size().height())
                .map_err(|error| error.to_string())?,
            source_id: selected.id().clone(),
            source_kind: selected.kind(),
            source_label: selected.name().to_owned(),
            settings,
        };
        let job = self
            .wayland_prepare_job
            .commit_crop(region, worker)
            .map_err(|error| error.to_string())?;
        self.settings.region_enabled = true;
        self.job = Some(job);
        self.progress = None;
        self.notice = Some("Wayland recording started from the prepared session…".to_owned());
        Ok(())
    }

    fn ensure_capture_source_catalog(&mut self) {
        if self.source_catalog_attempted {
            return;
        }
        self.source_catalog_attempted = true;
        match self.source_catalog_job.start() {
            Ok(()) => self.notice = Some("Loading Linux capture sources…".to_owned()),
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    fn receive_capture_source_result(&mut self) {
        if !self.source_catalog_job.drain() {
            return;
        }
        let Some(result) = self.source_catalog_job.take_result() else {
            self.notice = Some("Capture-source worker returned no result.".to_owned());
            return;
        };
        match result {
            Ok(catalog) => {
                let display_server = catalog.display_server();
                let sources = catalog.into_sources();
                let selected_source = sources
                    .iter()
                    .position(|source| !source.name().contains("(root)"))
                    .unwrap_or(0);
                self.display_server = Some(display_server);
                self.sources = sources;
                self.selected_source = selected_source;
                self.notice = Some(format!(
                    "Found {} {:?} capture source option(s).",
                    self.sources.len(),
                    display_server
                ));
            }
            Err(error) => {
                self.display_server = None;
                self.sources.clear();
                self.selected_source = 0;
                self.notice = Some(format!("Could not load Linux capture sources: {error}"));
            }
        }
    }

    fn begin_wayland_preparation(&mut self) -> Result<(), String> {
        if self.wayland_prepare_job.is_active() {
            return Ok(());
        }
        let source = self
            .sources
            .get(self.selected_source)
            .cloned()
            .ok_or_else(|| "No Wayland portal source is selected.".to_owned())?;
        self.wayland_frozen_preview = None;
        let cadence = recording_cadence(&self.settings)?;
        self.wayland_prepare_job
            .start(source, cadence)
            .map_err(|error| error.to_string())?;
        self.notice = Some(
            "Opening the Wayland system chooser in the background. Select a screen or window to prepare its frozen preview."
                .to_owned(),
        );
        Ok(())
    }

    fn receive_wayland_prepare_messages(&mut self, context: &egui::Context) {
        for event in self.wayland_prepare_job.drain() {
            match event {
                WaylandPrepareJobEvent::StateChanged(state) => {
                    self.notice = Some(wayland_prepare_state_notice(state).to_owned());
                }
                WaylandPrepareJobEvent::PreviewReady(preview) => {
                    match frozen_preview_image(&preview) {
                        Ok(image) => {
                            let source_size = preview.size();
                            let texture = context.load_texture(
                                "wayland-frozen-source-preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            );
                            let selection = initial_wayland_region(&self.settings, source_size);
                            apply_wayland_region_to_settings(&mut self.settings, selection);
                            self.wayland_frozen_preview = Some(WaylandFrozenPreview {
                                texture,
                                source_size,
                                selection,
                                drag_start: None,
                                drag_current: None,
                                drag_initial_region: None,
                            });
                            self.notice = Some(format!(
                                "Wayland source prepared at {}×{} pixels. The native session is paused and retained by its worker.",
                                source_size.width(),
                                source_size.height()
                            ));
                        }
                        Err(error) => {
                            let _ = self.wayland_prepare_job.cancel();
                            self.notice = Some(error);
                        }
                    }
                }
                WaylandPrepareJobEvent::Finished => {
                    self.wayland_frozen_preview = None;
                    if self.wayland_crop_controller.take().is_some() {
                        self.restore_main_window = true;
                    }
                    self.notice = Some(match self.wayland_prepare_job.take_result() {
                        Some(Ok(WaylandPrepareOutcome::Cancelled)) => {
                            "Wayland source preparation cancelled and its portal session closed."
                                .to_owned()
                        }
                        Some(Err(error)) => {
                            format!("Could not prepare Wayland source: {error}")
                        }
                        None => "Wayland preparation ended without a result.".to_owned(),
                    });
                }
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn show_wayland_preparation(&mut self, ui: &mut egui::Ui) {
        ui.heading("Wayland source preparation");
        let mut selected_from_drag = None;
        let mut apply_exact = false;
        let mut open_controller = false;
        if let Some(preview) = &mut self.wayland_frozen_preview {
            ui.label(format!(
                "Frozen {}×{} PipeWire frame",
                preview.source_size.width(),
                preview.source_size.height()
            ));
            ui.weak(
                "This is a source-local preview, not a window positioned over global desktop coordinates.",
            );
            ui.add_space(8.0);
            selected_from_drag = draw_wayland_region_selector(ui, preview, true);
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
                apply_exact = ui.button("Apply exact region").clicked();
            });
            ui.label(format!(
                "Selected {}×{} at {},{}",
                preview.selection.size().width(),
                preview.selection.size().height(),
                preview.selection.origin().x,
                preview.selection.origin().y
            ));
            open_controller = ui.button("Open source-local recorder controller").clicked();
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(wayland_prepare_state_notice(
                    self.wayland_prepare_job.state(),
                ));
            });
        }
        if let Some(region) = selected_from_drag {
            if let Some(preview) = &mut self.wayland_frozen_preview {
                preview.selection = region;
            }
            apply_wayland_region_to_settings(&mut self.settings, region);
        }
        if apply_exact {
            let result = PhysicalRect::new(
                self.settings.region_x,
                self.settings.region_y,
                self.settings.region_width,
                self.settings.region_height,
            )
            .map_err(|error| error.to_string())
            .and_then(|region| {
                let source_size = self
                    .wayland_frozen_preview
                    .as_ref()
                    .map(|preview| preview.source_size)
                    .ok_or_else(|| "Wayland preview is no longer available.".to_owned())?;
                region
                    .fits_within(source_size)
                    .then_some(region)
                    .ok_or_else(|| {
                        "The exact region must stay inside the frozen source frame.".to_owned()
                    })
            });
            match result {
                Ok(region) => {
                    if let Some(preview) = &mut self.wayland_frozen_preview {
                        preview.selection = region;
                    }
                    self.notice = Some("Exact Wayland source-local region applied.".to_owned());
                }
                Err(error) => self.notice = Some(error),
            }
        }
        if open_controller && let Err(error) = self.open_wayland_crop_controller(ui.ctx()) {
            self.notice = Some(error);
        }
        ui.add_space(8.0);
        let cancelling = self.wayland_prepare_job.state() == WaylandPrepareJobState::Cancelling;
        if ui
            .add_enabled(!cancelling, egui::Button::new("Cancel preparation"))
            .clicked()
            && self.wayland_prepare_job.cancel()
        {
            self.notice = Some(
                "Cancellation requested. A prepared session will close immediately; an open system chooser may still need to be dismissed."
                    .to_owned(),
            );
        }
    }

    fn open_wayland_crop_controller(&mut self, context: &egui::Context) -> Result<(), String> {
        if self.wayland_prepare_job.state() != WaylandPrepareJobState::Prepared {
            return Err("The Wayland source is not ready yet.".to_owned());
        }
        let snapshot = context
            .input(|input| {
                let viewport = input.viewport();
                Some(MainWindowSnapshot {
                    position: viewport.outer_rect.map(|rect| rect.min),
                    size: viewport.inner_rect?.size(),
                })
            })
            .ok_or_else(|| "Could not read the main window size.".to_owned())?;
        let preview = self
            .wayland_frozen_preview
            .take()
            .ok_or_else(|| "The frozen Wayland preview is no longer available.".to_owned())?;
        self.wayland_crop_controller = Some(WaylandCropController {
            texture: preview.texture,
            source_size: preview.source_size,
            region: preview.selection,
            drag_start: None,
            drag_current: None,
            drag_initial_region: None,
        });
        self.main_window_snapshot = Some(snapshot);
        self.notice = Some(
            "Source-local recorder controller opened. Its preview rectangle controls the crop; the window itself is not physically aligned to the desktop."
                .to_owned(),
        );
        context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        context.request_repaint();
        Ok(())
    }

    fn show_wayland_crop_controller(&mut self, context: &egui::Context) {
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
        if let Some(snapshot_notice) = self.job.as_mut().and_then(RecordingJob::poll_snapshots) {
            self.notice = Some(snapshot_notice);
        }
        let Some(mut controller) = self.wayland_crop_controller.take() else {
            return;
        };
        let progress = self.progress;
        let manual_snapshots = self.settings.cadence == RecordingCadenceChoice::Manual;
        let frame = context.show_viewport_immediate(
            recorder_viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("GifFromScreen Wayland crop controller")
                .with_inner_size([920.0, 650.0])
                .with_min_inner_size([420.0, 320.0])
                .with_decorations(true)
                .with_resizable(true)
                .with_always_on_top()
                .with_taskbar(false),
            |viewport_context, _class| {
                draw_wayland_crop_controller(
                    viewport_context,
                    stage,
                    progress,
                    &mut controller,
                    manual_snapshots,
                )
            },
        );
        let region = frame.region.unwrap_or(controller.region);
        controller.region = region;
        self.wayland_crop_controller = Some(controller);
        apply_overlay_region(&mut self.settings, stage, region);
        if should_sync_retarget(stage, frame.action)
            && let Some(job) = &mut self.job
        {
            job.observe_target(region);
        }
        self.handle_wayland_controller_action(context, frame.action);
    }

    fn handle_wayland_controller_action(
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
                    self.notice = Some("Recording countdown cancelled.".to_owned());
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
            RecorderOverlayAction::Snapshot => {
                if let Some(job) = &mut self.job {
                    job.trigger_snapshot();
                    self.notice = Some("Manual snapshot requested…".to_owned());
                    context.request_repaint();
                }
            }
            RecorderOverlayAction::Stop => {
                if let Some(job) = &mut self.job {
                    job.stop_retargeting();
                    let _ = job.controller.stop();
                    self.notice = Some("Stopping and finalizing recoverable project…".to_owned());
                }
            }
            RecorderOverlayAction::Discard | RecorderOverlayAction::Close => {
                if let Some(job) = &mut self.job {
                    job.stop_retargeting();
                    let _ = job.controller.discard();
                    job.cancellation.cancel();
                    self.notice = Some("Discarding recording…".to_owned());
                } else {
                    self.close_wayland_crop_controller();
                }
            }
        }
    }

    fn close_wayland_crop_controller(&mut self) {
        self.recording_countdown.cancel();
        self.wayland_crop_controller = None;
        let _ = self.wayland_prepare_job.cancel();
        self.restore_main_window = true;
    }

    fn refresh_sources(&mut self) {
        if self.source_catalog_job.state() == CaptureSourceJobState::Loading {
            self.notice = Some("Capture-source refresh is already running.".to_owned());
            return;
        }
        if self.wayland_prepare_job.is_active() {
            let _ = self.wayland_prepare_job.cancel();
            self.wayland_frozen_preview = None;
        }
        self.source_catalog_attempted = true;
        match self.source_catalog_job.start() {
            Ok(()) => self.notice = Some("Refreshing Linux capture sources…".to_owned()),
            Err(error) => self.notice = Some(error.to_string()),
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
                    self.notice = Some("Finalizing recoverable project…".to_owned());
                }
                JobMessage::Finished(RecordingCompletion::Completed(project)) => {
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
                JobMessage::Finished(RecordingCompletion::Discarded { cleanup_error }) => {
                    self.notice = Some(cleanup_error.map_or_else(
                        || "Recording discarded; its autosave project was removed.".to_owned(),
                        |error| format!("Recording discarded, but {error}"),
                    ));
                    self.finish_recording_job();
                }
                JobMessage::Finished(RecordingCompletion::Failed {
                    error,
                    recovery_path,
                }) => {
                    self.notice = Some(recovery_path.map_or_else(
                        || format!("Recording failed before autosave project creation: {error}"),
                        |path| {
                            format!(
                                "Recording failed: {error}. Recoverable autosave retained at {}",
                                path.display()
                            )
                        },
                    ));
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
            Some(Err(error)) => open_project_error_notice(&error),
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

    fn receive_import_image_messages(&mut self) {
        let finished = self
            .import_image_job
            .drain()
            .into_iter()
            .any(|event| event == ImportStaticImageJobEvent::Finished);
        if !finished {
            return;
        }
        let result = self.import_image_job.take_result();
        self.import_image_job = ImportStaticImageJob::default();
        self.notice = Some(match result {
            Some(Ok(project)) => match self.activate_imported_image(project) {
                Ok(notice) => notice,
                Err(error) => format!("Could not prepare imported image project: {error}"),
            },
            Some(Err(error)) => format!(
                "Could not import image: {error}. You can correct the path or file and retry."
            ),
            None => {
                "Image import worker finished without a result. You can retry safely.".to_owned()
            }
        });
    }

    fn activate_imported_image(&mut self, project: ActiveProject) -> Result<String, String> {
        let source = Path::new(self.import_image_path.trim());
        let output = source.with_extension("gif");
        let output_exists = output.exists();
        let summary = activate_editor(&mut self.view, &mut self.editor_workspace, project)?;
        self.editor_ui_state = EditorUiState::default();
        self.editor_preview_cache = EditorPreviewCache::new();
        self.editor_export_settings = EditorExportSettings::default();
        self.export_job = ExportJob::default();
        self.settings.output = output.to_string_lossy().into_owned();
        let existing = if output_exists {
            " The default GIF already exists; enable Overwrite before exporting."
        } else {
            ""
        };
        Ok(format!(
            "Imported image into {} as one 100 ms frame. Default GIF output is {}.{existing}",
            summary.project_path.display(),
            output.display()
        ))
    }

    fn receive_import_sequence_messages(&mut self) {
        let finished = self
            .import_sequence_job
            .drain()
            .into_iter()
            .any(|event| event == ImportStaticSequenceJobEvent::Finished);
        if !finished {
            return;
        }
        let result = self.import_sequence_job.take_result();
        self.import_sequence_job = ImportStaticSequenceJob::default();
        self.notice = Some(match result {
            Some(Ok(project)) => match self.activate_imported_sequence(project) {
                Ok(notice) => notice,
                Err(error) => format!("Could not prepare imported image sequence: {error}"),
            },
            Some(Err(error)) => format!(
                "Could not import image sequence: {error}. Adjust the ordered inputs or settings and retry."
            ),
            None => {
                "Image-sequence worker finished without a result. You can retry safely.".to_owned()
            }
        });
    }

    fn activate_imported_sequence(&mut self, project: ActiveProject) -> Result<String, String> {
        let output = project.layout().root.with_extension("gif");
        let output_exists = output.exists();
        let summary = activate_editor(&mut self.view, &mut self.editor_workspace, project)?;
        self.editor_ui_state = EditorUiState::default();
        self.editor_preview_cache = EditorPreviewCache::new();
        self.editor_export_settings = EditorExportSettings::default();
        self.export_job = ExportJob::default();
        self.import_sequence_ui = StaticSequenceUiState::default();
        self.settings.output = output.to_string_lossy().into_owned();
        let existing = if output_exists {
            " The default GIF already exists; enable Overwrite before exporting."
        } else {
            ""
        };
        Ok(format!(
            "Imported {} ordered images into {}. Default GIF output is {}.{existing}",
            summary.frames,
            summary.project_path.display(),
            output.display()
        ))
    }

    fn receive_blank_project_messages(&mut self) {
        let finished = self
            .blank_project_job
            .drain()
            .into_iter()
            .any(|event| event == BlankProjectJobEvent::Finished);
        if !finished {
            return;
        }
        let result = self.blank_project_job.take_result();
        self.blank_project_job = BlankProjectJob::default();
        self.notice = Some(match result {
            Some(Ok(project)) => match self.activate_blank_project(project) {
                Ok(notice) => notice,
                Err(error) => format!("Could not prepare the blank project editor: {error}"),
            },
            Some(Err(error)) => format!(
                "Could not create blank animation: {error}. Adjust the original form and retry."
            ),
            None => {
                "Blank-project worker finished without a result. You can retry safely.".to_owned()
            }
        });
    }

    fn receive_watermark_messages(&mut self) {
        let finished = self
            .watermark_job
            .drain()
            .into_iter()
            .any(|event| event == WatermarkDecodeEvent::Finished);
        if !finished {
            return;
        }
        let result = self.watermark_job.take_result();
        let pending = self.pending_watermark.take();
        self.watermark_job = WatermarkDecodeJob::default();
        self.notice = Some(match result {
            Some(Ok(decoded)) => {
                let source_path = decoded.source_path.clone();
                let source_size = decoded.size;
                match self
                    .editor_workspace
                    .as_mut()
                    .ok_or_else(|| "the editor project was closed while decoding".to_owned())
                    .and_then(|workspace| {
                        pending
                            .ok_or_else(|| {
                                "the watermark authoring target was lost; retry".to_owned()
                            })?
                            .commit(workspace, &decoded)
                    }) {
                    Ok(()) => format!(
                        "Added {}×{} watermark {} to the selected frame span.",
                        source_size.width.get(),
                        source_size.height.get(),
                        source_path.display()
                    ),
                    Err(error) => format!(
                        "Watermark decoded but could not be added: {error}. Adjust the form and retry."
                    ),
                }
            }
            Some(Err(error)) => {
                format!("Could not decode watermark: {error}. Adjust the form and retry.")
            }
            None => "Watermark decoder finished without a result; retry safely.".to_owned(),
        });
    }

    fn receive_text_messages(&mut self) {
        if let Some(notice) = self.text_overlay.poll(self.editor_workspace.as_mut()) {
            self.notice = Some(notice);
        }
    }

    fn activate_blank_project(&mut self, project: ActiveProject) -> Result<String, String> {
        let output = project.layout().root.with_extension("gif");
        let output_exists = output.exists();
        let summary = activate_editor(&mut self.view, &mut self.editor_workspace, project)?;
        self.editor_ui_state = EditorUiState::default();
        self.editor_preview_cache = EditorPreviewCache::new();
        self.editor_export_settings = EditorExportSettings::default();
        self.export_job = ExportJob::default();
        self.blank_project_ui = BlankProjectUiState::default();
        self.settings.output = output.to_string_lossy().into_owned();
        let existing = if output_exists {
            " The default GIF already exists; enable Overwrite before exporting."
        } else {
            ""
        };
        Ok(format!(
            "Blank animation ready at {} with one {:.3}s frame. Default GIF output is {}.{existing}",
            summary.project_path.display(),
            Duration::from_micros(summary.duration_us).as_secs_f64(),
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
        self.open_project_take_over_lock = false;
        self.settings.output = output.to_string_lossy().into_owned();
        self.view = AppView::Editor;
        Ok(notice)
    }

    fn finish_recording_job(&mut self) {
        self.job = None;
        self.recording_countdown.cancel();
        self.recorder_overlay = None;
        self.wayland_crop_controller = None;
        self.wayland_frozen_preview = None;
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

fn editor_result_notice(result: EditorUiResult) -> Option<String> {
    match result {
        Ok(EditorUiAction::Notice { message, .. }) => Some(message),
        Err(failure) => Some(format!(
            "Editor {:?} failed: {}",
            failure.operation, failure.message
        )),
        Ok(_) => None,
    }
}

fn show_editor_inspector(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    text: &mut TextOverlayTool,
    watermark: &mut WatermarkUiState,
    watermark_job: WatermarkDecodeJobState,
) -> (Vec<EditorUiResult>, Option<String>, WatermarkUiAction) {
    let mut results = Vec::new();
    let mut notice = None;
    let mut action = WatermarkUiAction::None;
    egui::ScrollArea::vertical()
        .id_salt("editor-inspector")
        .max_height(380.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            results = ui
                .add_enabled_ui(watermark_job != WatermarkDecodeJobState::Running, |ui| {
                    show_editor_tool_panel(ui, workspace, state)
                })
                .inner;
            if state.overlays_selected() && state.overlay_tool == OverlayTool::Text {
                notice = text.show(ui, workspace);
            }
            if state.overlays_selected() && state.overlay_tool == OverlayTool::Image {
                action = show_watermark_ui(
                    ui,
                    watermark,
                    watermark_job,
                    !workspace.selection().is_empty(),
                );
            }
        });
    (results, notice, action)
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
    state: &mut EditorUiState,
) {
    state.drawing_overlay.reconcile(workspace);
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
            let scale = (available_width / natural.x)
                .min(360.0 / natural.y)
                .min(3.0);
            let image_size = natural * scale;
            let sense = if state.drawing_overlay.phase == DrawingDraftPhase::Capturing {
                egui::Sense::drag()
            } else {
                egui::Sense::hover()
            };
            let response = ui.add(
                egui::Image::new(&preview.texture)
                    .fit_to_exact_size(image_size)
                    .sense(sense),
            );
            update_drawing_draft_from_preview(
                &response,
                preview.rendered_size,
                &mut state.drawing_overlay,
            );
            paint_drawing_draft(
                ui.painter(),
                response.rect,
                preview.rendered_size,
                &state.drawing_overlay,
            );
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

fn update_drawing_draft_from_preview(
    response: &egui::Response,
    rendered_size: [u32; 2],
    draft: &mut DrawingOverlayDraft,
) {
    if draft.phase != DrawingDraftPhase::Capturing {
        return;
    }
    if (response.drag_started() || response.dragged())
        && let Some(position) = response.interact_pointer_pos()
        && let Some(point) = map_drawing_preview_point(response.rect, position, rendered_size)
    {
        draft.push_point(StrokePoint {
            point,
            pressure_milli: 1_000,
        });
    }
    if response.drag_stopped() {
        draft.finish_stroke();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "finite preview coordinates are clamped to the validated u32 rendered canvas"
)]
fn map_drawing_preview_point(
    preview: egui::Rect,
    position: egui::Pos2,
    rendered_size: [u32; 2],
) -> Option<ProjectPhysicalPoint> {
    if preview.width() <= 0.0
        || preview.height() <= 0.0
        || rendered_size[0] == 0
        || rendered_size[1] == 0
        || !position.x.is_finite()
        || !position.y.is_finite()
    {
        return None;
    }
    let position = clamp_to_rect(position, preview);
    let normalized_x = ((position.x - preview.min.x) / preview.width()).clamp(0.0, 1.0);
    let normalized_y = ((position.y - preview.min.y) / preview.height()).clamp(0.0, 1.0);
    let x = (normalized_x * rendered_size[0] as f32).floor() as u32;
    let y = (normalized_y * rendered_size[1] as f32).floor() as u32;
    Some(ProjectPhysicalPoint {
        x: PhysicalPx::new(x.min(rendered_size[0] - 1)),
        y: PhysicalPx::new(y.min(rendered_size[1] - 1)),
    })
}

#[allow(
    clippy::cast_precision_loss,
    reason = "drawing drafts are bounded and mapped only into the small egui preview"
)]
fn paint_drawing_draft(
    painter: &egui::Painter,
    preview: egui::Rect,
    rendered_size: [u32; 2],
    draft: &DrawingOverlayDraft,
) {
    if draft.phase == DrawingDraftPhase::Idle
        || draft.points.is_empty()
        || rendered_size[0] == 0
        || rendered_size[1] == 0
    {
        return;
    }
    let points = draft
        .points
        .iter()
        .map(|point| {
            egui::pos2(
                preview.min.x
                    + (point.point.x.get() as f32 + 0.5) / rendered_size[0] as f32
                        * preview.width(),
                preview.min.y
                    + (point.point.y.get() as f32 + 0.5) / rendered_size[1] as f32
                        * preview.height(),
            )
        })
        .collect::<Vec<_>>();
    let color = egui::Color32::from_rgba_unmultiplied(
        draft.color.red,
        draft.color.green,
        draft.color.blue,
        draft.color.alpha,
    );
    let scale =
        (preview.width() / rendered_size[0] as f32).min(preview.height() / rendered_size[1] as f32);
    let stroke_width = (f32::from(draft.width) * scale).max(1.0);
    if points.len() == 1 {
        painter.circle_filled(points[0], stroke_width / 2.0, color);
    } else {
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(stroke_width, color),
        ));
    }
}

fn show_export_panel(
    ui: &mut egui::Ui,
    output: &mut String,
    settings: &mut EditorExportSettings,
    job: &ExportJob,
    selected_count: usize,
    asset_issue_count: usize,
    editor_mutation_active: bool,
) -> EditorExportAction {
    ui.heading("Export GIF");
    let active = export_job_is_active(job.state());
    ui.add_enabled_ui(!active && !editor_mutation_active, |ui| {
        show_export_configuration(ui, output, settings, selected_count);
    });
    if editor_mutation_active {
        ui.weak("Finish the active editor asset job before exporting.");
    }
    if asset_issue_count > 0 {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Resolve {asset_issue_count} asset issue(s) before exporting."),
        );
    }

    match job.state() {
        ExportJobState::Idle => {
            if ui
                .add_enabled(
                    asset_issue_count == 0 && !editor_mutation_active,
                    egui::Button::new("Export GIF"),
                )
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
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("editor_export_quantizer")
                    .selected_text(export_quantizer_label(settings.quantizer))
                    .show_ui(ui, |ui| {
                        for choice in [
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
                            ui.selectable_value(
                                &mut settings.quantizer,
                                choice,
                                export_quantizer_label(choice),
                            );
                        }
                    });
                if let Some(required) = fixed_palette_required_colors(settings.quantizer) {
                    ui.weak(format!(
                        "Fixed palette · at least {required} colors including transparency"
                    ));
                }
            });
            ui.end_row();

            if settings.quantizer == ExportQuantizerChoice::Custom {
                ui.label("Custom colors");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut settings.custom_palette_text)
                            .code_editor()
                            .desired_rows(4)
                            .desired_width(430.0)
                            .hint_text("#000000, #FFFFFF"),
                    );
                    ui.weak(
                        "2..=256 strict #RRGGBB entries separated by commas or whitespace; count must not exceed Maximum colors.",
                    );
                });
                ui.end_row();

                ui.label("Custom transparency");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut settings.custom_transparency_enabled,
                            "Use transparent palette index",
                        );
                        ui.add_enabled(
                            settings.custom_transparency_enabled,
                            egui::DragValue::new(&mut settings.custom_transparent_index)
                                .range(0..=u16::from(u8::MAX)),
                        );
                        ui.weak("zero-based");
                    });
                    ui.weak("Required when rendered pixels cross the alpha threshold.");
                });
                ui.end_row();
            }

            ui.label("Dither");
            egui::ComboBox::from_id_salt("editor_export_dither")
                .selected_text(export_dither_label(settings.dither))
                .show_ui(ui, |ui| {
                    for choice in [
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
        ExportQuantizerChoice::Wu => "Wu variance",
        ExportQuantizerChoice::Grayscale => "Grayscale",
        ExportQuantizerChoice::MostUsed => "Most used",
        ExportQuantizerChoice::NeuQuant => "NeuQuant",
        ExportQuantizerChoice::WebSafe216 => "Web safe 216 (fixed)",
        ExportQuantizerChoice::Monochrome => "Monochrome (fixed)",
        ExportQuantizerChoice::Windows16 => "Windows 16 (fixed)",
        ExportQuantizerChoice::Custom => "Custom palette",
    }
}

const fn export_dither_label(choice: ExportDitherChoice) -> &'static str {
    match choice {
        ExportDitherChoice::None => "None",
        ExportDitherChoice::Bayer => "Bayer 4×4",
        ExportDitherChoice::Dotted => "Dotted halftone",
        ExportDitherChoice::BlueNoise => "Blue noise",
        ExportDitherChoice::InterleavedNoise => "Interleaved gradient noise",
        ExportDitherChoice::FloydSteinberg => "Floyd–Steinberg",
        ExportDitherChoice::Atkinson => "Atkinson",
        ExportDitherChoice::Burkes => "Burkes",
        ExportDitherChoice::Sierra => "Sierra",
        ExportDitherChoice::SierraLite => "Sierra Lite",
        ExportDitherChoice::TwoRowSierra => "Two-row Sierra",
        ExportDitherChoice::JarvisJudiceNinke => "Jarvis–Judice–Ninke",
        ExportDitherChoice::Stucki => "Stucki",
        ExportDitherChoice::StevensonArce => "Stevenson–Arce",
    }
}

fn draw_wayland_crop_controller(
    context: &egui::Context,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    controller: &mut WaylandCropController,
    manual_snapshots: bool,
) -> RecorderOverlayFrame {
    let mut action =
        draw_wayland_controller_toolbar(context, stage, progress, controller, manual_snapshots);
    let region = egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(egui::Color32::from_rgb(16, 18, 22)))
        .show(context, |ui| {
            ui.label(
                "Frozen source-local preview — this controller window is not a physical desktop frame.",
            );
            draw_source_region_selector(
                ui,
                &controller.texture,
                controller.source_size,
                controller.region,
                &mut controller.drag_start,
                &mut controller.drag_current,
                &mut controller.drag_initial_region,
                stage.allows_resizing(),
            )
        })
        .inner;
    if context.input(|input| input.viewport().close_requested()) {
        action = RecorderOverlayAction::Close;
    }
    RecorderOverlayFrame { action, region }
}

fn draw_wayland_controller_toolbar(
    context: &egui::Context,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    controller: &mut WaylandCropController,
    manual_snapshots: bool,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    egui::TopBottomPanel::bottom("wayland_crop_controls")
        .exact_height(92.0)
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(28, 30, 34))
                .inner_margin(8),
        )
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "Crop {}×{} at {},{}",
                    controller.region.size().width(),
                    controller.region.size().height(),
                    controller.region.origin().x,
                    controller.region.origin().y
                ));
                show_wayland_crop_nudges(ui, controller);
            });
            ui.horizontal(|ui| match stage {
                RecorderStage::Ready => {
                    ui.label("Drag a new rectangle to resize before recording.");
                    if ui.button("Start").clicked() {
                        action = RecorderOverlayAction::Start;
                    }
                    if ui.button("Cancel").clicked() {
                        action = RecorderOverlayAction::Close;
                    }
                }
                RecorderStage::Countdown(remaining) => {
                    ui.strong(format!("Recording starts in {remaining}s"));
                    if ui.button("Cancel countdown").clicked() {
                        action = RecorderOverlayAction::CancelCountdown;
                    }
                }
                RecorderStage::Recording => {
                    show_overlay_progress(ui, progress);
                    if manual_snapshots && ui.button("Take snapshot").clicked() {
                        action = RecorderOverlayAction::Snapshot;
                    }
                    if ui.button("Pause").clicked() {
                        action = RecorderOverlayAction::Pause;
                    }
                    if ui.button("Stop").clicked() {
                        action = RecorderOverlayAction::Stop;
                    }
                    if ui.button("Discard").clicked() {
                        action = RecorderOverlayAction::Discard;
                    }
                }
                RecorderStage::Paused => {
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
                }
                RecorderStage::Finalizing => {
                    ui.spinner();
                    ui.label("Finalizing recoverable project…");
                    if ui.button("Cancel").clicked() {
                        action = RecorderOverlayAction::Discard;
                    }
                }
            });
        });
    action
}

fn show_wayland_crop_nudges(ui: &mut egui::Ui, controller: &mut WaylandCropController) {
    for (label, dx, dy) in [("←", -10, 0), ("→", 10, 0), ("↑", 0, -10), ("↓", 0, 10)] {
        if ui.small_button(label).clicked() {
            controller.region =
                translate_source_region(controller.region, controller.source_size, dx, dy);
        }
    }
    ui.weak("10 px source-local");
}

fn draw_wayland_region_selector(
    ui: &mut egui::Ui,
    preview: &mut WaylandFrozenPreview,
    allow_resize: bool,
) -> Option<PhysicalRect> {
    draw_source_region_selector(
        ui,
        &preview.texture,
        preview.source_size,
        preview.selection,
        &mut preview.drag_start,
        &mut preview.drag_current,
        &mut preview.drag_initial_region,
        allow_resize,
    )
}

fn initial_wayland_region(
    settings: &RecordingSettings,
    source_size: gif_from_screen_capture::PhysicalSize,
) -> PhysicalRect {
    if !settings.region_enabled {
        return PhysicalRect::new(0, 0, source_size.width(), source_size.height())
            .expect("a captured source has non-zero dimensions");
    }
    let width = settings.region_width.clamp(1, source_size.width());
    let height = settings.region_height.clamp(1, source_size.height());
    let maximum_x = i32::try_from(source_size.width() - width).unwrap_or(i32::MAX);
    let maximum_y = i32::try_from(source_size.height() - height).unwrap_or(i32::MAX);
    PhysicalRect::new(
        settings.region_x.clamp(0, maximum_x),
        settings.region_y.clamp(0, maximum_y),
        width,
        height,
    )
    .expect("clamped source-local region is non-empty")
}

fn apply_wayland_region_to_settings(settings: &mut RecordingSettings, region: PhysicalRect) {
    settings.region_enabled = true;
    settings.region_x = region.origin().x;
    settings.region_y = region.origin().y;
    settings.region_width = region.size().width();
    settings.region_height = region.size().height();
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn draw_source_region_selector(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    source_size: gif_from_screen_capture::PhysicalSize,
    region: PhysicalRect,
    drag_start: &mut Option<egui::Pos2>,
    drag_current: &mut Option<egui::Pos2>,
    drag_initial_region: &mut Option<PhysicalRect>,
    allow_resize: bool,
) -> Option<PhysicalRect> {
    let available = ui.available_size();
    let texture_size = texture.size_vec2();
    let scale = (available.x.max(1.0) / texture_size.x)
        .min((available.y - 16.0).max(1.0) / texture_size.y)
        .min(1.0);
    let response = ui.add(
        egui::Image::new(texture)
            .fit_to_exact_size(texture_size * scale)
            .sense(egui::Sense::drag()),
    );
    if response.drag_started() {
        *drag_start = response.interact_pointer_pos();
        *drag_current = *drag_start;
        *drag_initial_region = Some(region);
    }
    if response.dragged() {
        *drag_current = response.interact_pointer_pos();
    }
    let candidate = match (*drag_start, *drag_current, *drag_initial_region) {
        (Some(start), Some(current), Some(_initial)) if allow_resize => {
            let selection = egui::Rect::from_two_pos(
                clamp_to_rect(start, response.rect),
                clamp_to_rect(current, response.rect),
            );
            map_preview_selection(
                response.rect,
                selection,
                source_size.width(),
                source_size.height(),
            )
        }
        (Some(start), Some(current), Some(initial)) => {
            let delta = current - start;
            let dx = (delta.x / response.rect.width() * source_size.width() as f32).round() as i32;
            let dy =
                (delta.y / response.rect.height() * source_size.height() as f32).round() as i32;
            Some(translate_source_region(initial, source_size, dx, dy))
        }
        _ => None,
    };
    let displayed = candidate.unwrap_or(region);
    let selection_rect = source_region_on_preview(response.rect, displayed, source_size);
    ui.painter().rect_stroke(
        selection_rect,
        0.0,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(242, 153, 74)),
        egui::StrokeKind::Inside,
    );
    if response.drag_stopped() {
        *drag_start = None;
        *drag_current = None;
        *drag_initial_region = None;
        candidate
    } else {
        None
    }
}

#[allow(clippy::cast_precision_loss)]
fn source_region_on_preview(
    preview: egui::Rect,
    region: PhysicalRect,
    source_size: gif_from_screen_capture::PhysicalSize,
) -> egui::Rect {
    let left =
        preview.min.x + region.origin().x as f32 / source_size.width() as f32 * preview.width();
    let top =
        preview.min.y + region.origin().y as f32 / source_size.height() as f32 * preview.height();
    let width = region.size().width() as f32 / source_size.width() as f32 * preview.width();
    let height = region.size().height() as f32 / source_size.height() as f32 * preview.height();
    egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height))
}

fn translate_source_region(
    region: PhysicalRect,
    source_size: gif_from_screen_capture::PhysicalSize,
    dx: i32,
    dy: i32,
) -> PhysicalRect {
    let maximum_x = i64::from(source_size.width().saturating_sub(region.size().width()));
    let maximum_y = i64::from(source_size.height().saturating_sub(region.size().height()));
    let x = (i64::from(region.origin().x) + i64::from(dx)).clamp(0, maximum_x);
    let y = (i64::from(region.origin().y) + i64::from(dy)).clamp(0, maximum_y);
    PhysicalRect::new(
        i32::try_from(x).unwrap_or(i32::MAX),
        i32::try_from(y).unwrap_or(i32::MAX),
        region.size().width(),
        region.size().height(),
    )
    .unwrap_or(region)
}

fn draw_recorder_overlay(
    context: &egui::Context,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    source_geometry: PhysicalRect,
    manual_snapshots: bool,
) -> RecorderOverlayFrame {
    let mut action = draw_recorder_toolbar(context, stage, progress, manual_snapshots);
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
    manual_snapshots: bool,
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
                    if manual_snapshots && ui.button("Take snapshot").clicked() {
                        action = RecorderOverlayAction::Snapshot;
                    }
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
                    ui.label("Finalizing recoverable project…");
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

const fn wayland_prepare_state_notice(state: WaylandPrepareJobState) -> &'static str {
    match state {
        WaylandPrepareJobState::Idle => "Wayland source preparation is idle.",
        WaylandPrepareJobState::Connecting => {
            "Connecting to the Wayland ScreenCast portal in the background…"
        }
        WaylandPrepareJobState::Choosing => {
            "Choose a screen or window in the trusted system dialog…"
        }
        WaylandPrepareJobState::WaitingForFrame => {
            "The portal selection is ready; waiting for the first mapped PipeWire frame…"
        }
        WaylandPrepareJobState::Prepared => {
            "The frozen preview is ready and its native session is paused."
        }
        WaylandPrepareJobState::Cancelling => {
            "Cancellation requested. If the trusted chooser is still open, close it to finish portal teardown."
        }
        WaylandPrepareJobState::Finished => "Wayland source preparation finished.",
    }
}

fn frozen_preview_image(preview: &FrozenSourcePreview) -> Result<egui::ColorImage, String> {
    const MAX_PREVIEW_WIDTH: u32 = 1_600;
    const MAX_PREVIEW_HEIGHT: u32 = 900;

    let source_size = preview.size();
    let (width, height) = fit_dimensions(
        source_size.width(),
        source_size.height(),
        MAX_PREVIEW_WIDTH,
        MAX_PREVIEW_HEIGHT,
    );
    let resized;
    let rgba = if (width, height) == (source_size.width(), source_size.height()) {
        preview.rgba()
    } else {
        resized = resize_nearest_rgba(
            preview.rgba(),
            source_size.width(),
            source_size.height(),
            width,
            height,
        )?;
        &resized
    };
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [
            usize::try_from(width).map_err(|_| "preview width is too large")?,
            usize::try_from(height).map_err(|_| "preview height is too large")?,
        ],
        rgba,
    ))
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

fn recording_project_canvas(
    settings: &RecordingSettings,
    source_geometry: Option<PhysicalRect>,
) -> Result<ProjectPhysicalSize, String> {
    let (width, height) = if settings.region_enabled {
        (settings.region_width, settings.region_height)
    } else {
        let geometry = source_geometry
            .ok_or_else(|| "The selected source has no known project canvas size.".to_owned())?;
        (geometry.size().width(), geometry.size().height())
    };
    ProjectPhysicalSize::new(width, height).map_err(|error| error.to_string())
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
    let custom_palette = if settings.quantizer == ExportQuantizerChoice::Custom {
        let transparent_index = settings
            .custom_transparency_enabled
            .then_some(settings.custom_transparent_index);
        let palette = parse_custom_palette(&settings.custom_palette_text, transparent_index)
            .map_err(|error| format!("Invalid custom palette: {error}"))?;
        if palette.color_count() > usize::from(settings.max_colors) {
            return Err(format!(
                "Custom palette contains {} colors, above Maximum colors {}.",
                palette.color_count(),
                settings.max_colors
            ));
        }
        Some(palette)
    } else {
        None
    };
    if let Some(required) = fixed_palette_required_colors(settings.quantizer)
        && settings.max_colors < required
    {
        return Err(format!(
            "The selected fixed palette requires at least {required} colors including transparency."
        ));
    }
    if settings.loop_choice == ExportLoopChoice::Finite && settings.finite_loop_count == 0 {
        return Err("Finite loop count must be at least one.".to_owned());
    }
    let palette_mode = match settings.palette {
        ExportPaletteChoice::Local => PaletteMode::LocalPerFrame,
        ExportPaletteChoice::Global => PaletteMode::Global,
    };
    let quantizer = match settings.quantizer {
        // A validated custom palette bypasses adaptive quantization. Keep a
        // deterministic fallback in the encoder options for auditability.
        ExportQuantizerChoice::MedianCut | ExportQuantizerChoice::Custom => {
            QuantizerStrategy::MedianCut
        }
        ExportQuantizerChoice::Octree => QuantizerStrategy::Octree,
        ExportQuantizerChoice::Wu => QuantizerStrategy::Wu,
        ExportQuantizerChoice::Grayscale => QuantizerStrategy::Grayscale,
        ExportQuantizerChoice::MostUsed => QuantizerStrategy::MostUsed,
        ExportQuantizerChoice::NeuQuant => QuantizerStrategy::NeuQuant,
        ExportQuantizerChoice::WebSafe216 => QuantizerStrategy::WebSafe216,
        ExportQuantizerChoice::Monochrome => QuantizerStrategy::Monochrome,
        ExportQuantizerChoice::Windows16 => QuantizerStrategy::Windows16,
    };
    let dither = match settings.dither {
        ExportDitherChoice::None => DitherMode::None,
        ExportDitherChoice::Bayer => DitherMode::Bayer4x4,
        ExportDitherChoice::Dotted => DitherMode::Dotted,
        ExportDitherChoice::BlueNoise => DitherMode::BlueNoise,
        ExportDitherChoice::InterleavedNoise => DitherMode::InterleavedNoise,
        ExportDitherChoice::FloydSteinberg => DitherMode::FloydSteinberg,
        ExportDitherChoice::Atkinson => DitherMode::Atkinson,
        ExportDitherChoice::Burkes => DitherMode::Burkes,
        ExportDitherChoice::Sierra => DitherMode::Sierra,
        ExportDitherChoice::SierraLite => DitherMode::SierraLite,
        ExportDitherChoice::TwoRowSierra => DitherMode::TwoRowSierra,
        ExportDitherChoice::JarvisJudiceNinke => DitherMode::JarvisJudiceNinke,
        ExportDitherChoice::Stucki => DitherMode::Stucki,
        ExportDitherChoice::StevensonArce => DitherMode::StevensonArce,
    };
    let loop_behavior = match settings.loop_choice {
        ExportLoopChoice::Infinite => LoopBehavior::Infinite,
        ExportLoopChoice::Finite => LoopBehavior::Finite(settings.finite_loop_count),
    };
    Ok(ProjectGifExportOptions {
        frames,
        custom_palette,
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

const fn fixed_palette_required_colors(choice: ExportQuantizerChoice) -> Option<u16> {
    match choice {
        ExportQuantizerChoice::WebSafe216 => Some(217),
        ExportQuantizerChoice::Monochrome => Some(3),
        ExportQuantizerChoice::Windows16 => Some(17),
        ExportQuantizerChoice::MedianCut
        | ExportQuantizerChoice::Octree
        | ExportQuantizerChoice::Wu
        | ExportQuantizerChoice::Grayscale
        | ExportQuantizerChoice::MostUsed
        | ExportQuantizerChoice::NeuQuant
        | ExportQuantizerChoice::Custom => None,
    }
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

const fn open_project_controls_enabled(state: OpenProjectJobState) -> bool {
    !matches!(state, OpenProjectJobState::Running)
}

const fn open_project_lock_policy(take_over_lock: bool) -> LockPolicy {
    if take_over_lock {
        LockPolicy::TakeOver
    } else {
        LockPolicy::FailIfPresent
    }
}

fn open_project_error_notice(error: &OpenProjectJobError) -> String {
    match error {
        OpenProjectJobError::Project(source) => match source.as_ref() {
            ProjectError::AlreadyLocked { path, owner } => {
                let owner = owner.as_deref().map_or_else(
                    || "owner metadata is unavailable".to_owned(),
                    |owner| format!("owner: {owner}"),
                );
                format!(
                    "Project {} is already locked ({owner}). Verify that process has stopped, then explicitly confirm stale-lock takeover to retry.",
                    path.display()
                )
            }
            _ => format!("Could not open project: {source}"),
        },
        OpenProjectJobError::WorkerExited => error.to_string(),
    }
}

const fn can_navigate_back(
    view: AppView,
    open_state: OpenProjectJobState,
    import_state: ImportGifJobState,
    image_state: ImportStaticImageJobState,
    sequence_state: ImportStaticSequenceJobState,
    blank_state: BlankProjectJobState,
) -> bool {
    !((matches!(view, AppView::OpenProject) && matches!(open_state, OpenProjectJobState::Running))
        || (matches!(view, AppView::ImportGif)
            && matches!(import_state, ImportGifJobState::Running))
        || (matches!(view, AppView::ImportImage)
            && matches!(image_state, ImportStaticImageJobState::Running))
        || (matches!(view, AppView::ImportImageSequence)
            && matches!(sequence_state, ImportStaticSequenceJobState::Running))
        || (matches!(view, AppView::NewBlankAnimation)
            && matches!(blank_state, BlankProjectJobState::Running)))
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

fn default_sequence_project_path(inputs: &[PathBuf]) -> Result<PathBuf, String> {
    let first = inputs.first().ok_or_else(|| {
        "An image sequence needs at least one path to derive a target.".to_owned()
    })?;
    let stem = first
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| format!("Cannot derive a sequence target from {}.", first.display()))?;
    let parent = first
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = stem.to_string_lossy();
    for suffix in 1..=MAX_STATIC_SEQUENCE_FRAMES {
        let filename = if suffix == 1 {
            format!("{stem}-sequence.gfsproj")
        } else {
            format!("{stem}-sequence-{suffix}.gfsproj")
        };
        let candidate = parent.join(filename);
        match candidate.try_exists() {
            Ok(false) => return Ok(candidate),
            Ok(true) => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect default sequence target {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    Err(format!(
        "No available default sequence target remains beside {}.",
        first.display()
    ))
}

fn build_static_sequence_request(
    state: &StaticSequenceUiState,
) -> Result<ImportStaticSequenceRequest, String> {
    if state.inputs.len() < 2 {
        return Err("Add at least two images to the sequence.".to_owned());
    }
    if state.inputs.len() > MAX_STATIC_SEQUENCE_FRAMES {
        return Err(format!(
            "A sequence may contain at most {MAX_STATIC_SEQUENCE_FRAMES} images."
        ));
    }
    let target = state.target.trim();
    if target.is_empty() {
        return Err("Choose a .gfsproj target directory.".to_owned());
    }
    let duration_policy = static_sequence_duration_policy(state.timing, state.uniform_duration_ms)?;
    let loop_behavior = static_sequence_loop_behavior(state.loop_choice, state.finite_repeats)?;
    let mut inputs = Vec::new();
    let mut frame_ids = Vec::new();
    let mut display_names = Vec::new();
    inputs
        .try_reserve_exact(state.inputs.len())
        .map_err(|_| "Could not reserve sequence input paths.".to_owned())?;
    frame_ids
        .try_reserve_exact(state.inputs.len())
        .map_err(|_| "Could not reserve sequence frame identities.".to_owned())?;
    display_names
        .try_reserve_exact(state.inputs.len())
        .map_err(|_| "Could not reserve sequence source labels.".to_owned())?;
    for path in &state.inputs {
        let display_name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("Image path {} has no filename.", path.display()))?;
        inputs.push(path.clone());
        frame_ids.push(FrameId::from_u128(Uuid::new_v4().as_u128()));
        display_names.push(display_name.to_string_lossy().into_owned());
    }
    Ok(ImportStaticSequenceRequest {
        inputs,
        target: PathBuf::from(target),
        duration_policy,
        loop_behavior,
        limits: DecodeLimits {
            max_width: MAX_STATIC_SEQUENCE_EDGE,
            max_height: MAX_STATIC_SEQUENCE_EDGE,
            max_frames: MAX_STATIC_SEQUENCE_FRAMES,
            max_total_rgba_bytes: MAX_STATIC_SEQUENCE_RGBA_BYTES,
        },
        project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
        frame_ids,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: current_project_created_at()?,
        display_names,
    })
}

fn build_blank_project_request(state: &BlankProjectUiState) -> Result<BlankProjectRequest, String> {
    let target = state.target.trim();
    if target.is_empty() {
        return Err("Choose a new .gfsproj target directory.".to_owned());
    }
    if state.width > u32::from(u16::MAX) || state.height > u32::from(u16::MAX) {
        return Err("Blank canvas width and height must not exceed 65,535 pixels.".to_owned());
    }
    let canvas = ProjectPhysicalSize::new(state.width, state.height)
        .map_err(|error| format!("Blank canvas is invalid: {error}"))?;
    let duration_us = state
        .frame_duration_ms
        .checked_mul(1_000)
        .and_then(DurationUs::new)
        .ok_or_else(|| "Initial frame duration must be positive and fit u64.".to_owned())?;
    let background = match state.background_choice {
        BlankBackgroundChoice::Transparent => Rgba::TRANSPARENT,
        BlankBackgroundChoice::Solid if state.alpha > 0 => Rgba {
            red: state.red,
            green: state.green,
            blue: state.blue,
            alpha: state.alpha,
        },
        BlankBackgroundChoice::Solid => {
            return Err(
                "Solid background alpha must be at least 1; choose Transparent for alpha 0."
                    .to_owned(),
            );
        }
    };
    Ok(BlankProjectRequest {
        target: PathBuf::from(target),
        canvas,
        background,
        frame_duration: duration_us,
        frame_limit_bytes: DEFAULT_BLANK_FRAME_LIMIT_BYTES,
        project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
        frame_id: FrameId::from_u128(Uuid::new_v4().as_u128()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: current_project_created_at()?,
    })
}

fn current_project_created_at() -> Result<UnixTimeMs, String> {
    let created_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock is before the Unix epoch: {error}"))?
        .as_millis();
    let created_millis = i64::try_from(created_millis)
        .map_err(|_| "Current time does not fit the project timestamp.".to_owned())?;
    Ok(UnixTimeMs::new(created_millis))
}

fn static_sequence_duration_policy(
    timing: StaticSequenceTimingChoice,
    uniform_duration_ms: u64,
) -> Result<StaticImageSequenceDurationPolicy, String> {
    match timing {
        StaticSequenceTimingChoice::PreserveDefault => {
            Ok(StaticImageSequenceDurationPolicy::PreserveDecoded)
        }
        StaticSequenceTimingChoice::Uniform => {
            let duration_us = uniform_duration_ms
                .checked_mul(1_000)
                .and_then(NonZeroU64::new)
                .ok_or_else(|| "Uniform frame duration must be positive and fit u64.".to_owned())?;
            Ok(StaticImageSequenceDurationPolicy::Uniform(duration_us))
        }
    }
}

fn static_sequence_loop_behavior(
    choice: StaticSequenceLoopChoice,
    finite_repeats: u16,
) -> Result<ImportedLoopBehavior, String> {
    match choice {
        StaticSequenceLoopChoice::Once => Ok(ImportedLoopBehavior::Once),
        StaticSequenceLoopChoice::Infinite => Ok(ImportedLoopBehavior::Infinite),
        StaticSequenceLoopChoice::Finite if finite_repeats > 0 => {
            Ok(ImportedLoopBehavior::Finite(finite_repeats))
        }
        StaticSequenceLoopChoice::Finite => {
            Err("Finite sequence repeats must be at least one.".to_owned())
        }
    }
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
    drop(project);
    remove_recording_project_path(&project_path)
}

fn remove_recording_project_path(project_path: &Path) -> Result<(), String> {
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
    fs::remove_dir_all(project_path).map_err(|error| {
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
    let _ = recording_cadence(settings)?;
    let _ = recording_tail_frame_duration(settings)?;
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

const fn recording_cadence_label(choice: RecordingCadenceChoice) -> &'static str {
    match choice {
        RecordingCadenceChoice::FixedFps => "Continuous FPS",
        RecordingCadenceChoice::Periodic => "Periodic snapshots",
        RecordingCadenceChoice::Manual => "Manual snapshots",
    }
}

const fn recording_interval_unit_label(unit: RecordingIntervalUnit) -> &'static str {
    match unit {
        RecordingIntervalUnit::Seconds => "seconds",
        RecordingIntervalUnit::Minutes => "minutes",
        RecordingIntervalUnit::Hours => "hours",
    }
}

fn show_recording_cadence_settings(ui: &mut egui::Ui, settings: &mut RecordingSettings) {
    ui.label("Capture frequency");
    egui::ComboBox::from_id_salt("recording_cadence")
        .selected_text(recording_cadence_label(settings.cadence))
        .show_ui(ui, |ui| {
            for choice in [
                RecordingCadenceChoice::FixedFps,
                RecordingCadenceChoice::Periodic,
                RecordingCadenceChoice::Manual,
            ] {
                ui.selectable_value(
                    &mut settings.cadence,
                    choice,
                    recording_cadence_label(choice),
                );
            }
        });
    ui.end_row();
    match settings.cadence {
        RecordingCadenceChoice::FixedFps => {
            ui.label("Frames per second");
            ui.add(egui::DragValue::new(&mut settings.fps).range(1..=60));
            ui.end_row();
        }
        RecordingCadenceChoice::Periodic => {
            ui.label("Periodic snapshot interval");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut settings.interval_count).range(1..=10_000));
                egui::ComboBox::from_id_salt("recording_interval_unit")
                    .selected_text(recording_interval_unit_label(settings.interval_unit))
                    .show_ui(ui, |ui| {
                        for unit in [
                            RecordingIntervalUnit::Seconds,
                            RecordingIntervalUnit::Minutes,
                            RecordingIntervalUnit::Hours,
                        ] {
                            ui.selectable_value(
                                &mut settings.interval_unit,
                                unit,
                                recording_interval_unit_label(unit),
                            );
                        }
                    });
            });
            ui.end_row();
        }
        RecordingCadenceChoice::Manual => {
            ui.label("Final manual frame duration (ms)");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::DragValue::new(&mut settings.manual_frame_duration_ms)
                        .range(1..=MAX_RECORDING_DURATION_MS),
                );
                ui.weak("Earlier frame durations follow the time between snapshot clicks.");
            });
            ui.end_row();
        }
    }
}

fn show_frame_retention_setting(ui: &mut egui::Ui, settings: &mut RecordingSettings) {
    ui.label("Frame retention");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(settings.cadence != RecordingCadenceChoice::Manual, |ui| {
            ui.checkbox(
                &mut settings.changes_only,
                "Store only frames whose pixels changed",
            );
        });
        if settings.cadence == RecordingCadenceChoice::Manual {
            ui.weak("Every manual trigger is retained, including identical pixels.");
        }
    });
    ui.end_row();
}

fn recording_period(settings: &RecordingSettings) -> Result<Duration, String> {
    if settings.interval_count == 0 {
        return Err("Periodic snapshot interval must be at least one unit.".to_owned());
    }
    let seconds_per_unit = match settings.interval_unit {
        RecordingIntervalUnit::Seconds => 1_u64,
        RecordingIntervalUnit::Minutes => 60,
        RecordingIntervalUnit::Hours => 3_600,
    };
    let seconds = u64::from(settings.interval_count)
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| "Periodic snapshot interval is too large.".to_owned())?;
    Ok(Duration::from_secs(seconds))
}

fn recording_cadence(settings: &RecordingSettings) -> Result<CaptureCadence, String> {
    match settings.cadence {
        RecordingCadenceChoice::FixedFps => {
            if !(1..=60).contains(&settings.fps) {
                return Err("FPS must be between 1 and 60.".to_owned());
            }
            CaptureCadence::fixed_fps(settings.fps).map_err(|error| error.to_string())
        }
        RecordingCadenceChoice::Periodic => {
            CaptureCadence::interval(recording_period(settings)?).map_err(|error| error.to_string())
        }
        RecordingCadenceChoice::Manual => Ok(CaptureCadence::Manual),
    }
}

fn recording_tail_frame_duration(settings: &RecordingSettings) -> Result<Duration, String> {
    match settings.cadence {
        RecordingCadenceChoice::FixedFps => {
            if !(1..=60).contains(&settings.fps) {
                return Err("FPS must be between 1 and 60.".to_owned());
            }
            Ok(Duration::from_micros(1_000_000 / u64::from(settings.fps)))
        }
        RecordingCadenceChoice::Periodic => recording_period(settings),
        RecordingCadenceChoice::Manual => {
            if settings.manual_frame_duration_ms == 0
                || settings.manual_frame_duration_ms > MAX_RECORDING_DURATION_MS
            {
                return Err(format!(
                    "Final manual frame duration must be between 1 and {MAX_RECORDING_DURATION_MS} ms."
                ));
            }
            Ok(Duration::from_millis(settings.manual_frame_duration_ms))
        }
    }
}

fn run_incremental_x11_recording(
    worker: &RecordingWorkerRequest,
    control: &mut RecordingControl,
    cancellation: &CancellationFlag,
    progress: &mut dyn gif_from_screen_workflow::WorkflowProgressSink,
    on_finalizing: impl FnOnce(),
) -> RecordingCompletion {
    if cancellation.is_cancelled() {
        return RecordingCompletion::Discarded {
            cleanup_error: None,
        };
    }
    let project = match create_incremental_recording_project(worker) {
        Ok(project) => project,
        Err(error) => {
            return RecordingCompletion::Failed {
                error: error.to_string(),
                recovery_path: None,
            };
        }
    };
    let mut sink = IncrementalProjectFrameSink::new(project);
    match collect_x11_recording(worker, control, &mut sink, cancellation, progress) {
        Ok(()) => {
            on_finalizing();
            let recovery_path = sink.root().to_path_buf();
            match sink.finish() {
                Ok(project) => RecordingCompletion::Completed(Box::new(project)),
                Err(error) => RecordingCompletion::Failed {
                    error: error.to_string(),
                    recovery_path: Some(recovery_path),
                },
            }
        }
        Err(WorkflowError::Discarded | WorkflowError::Cancelled) => {
            let project_path = sink.root().to_path_buf();
            drop(sink);
            RecordingCompletion::Discarded {
                cleanup_error: remove_recording_project_path(&project_path).err(),
            }
        }
        Err(error) => {
            let recovery_path = sink.root().to_path_buf();
            drop(sink);
            RecordingCompletion::Failed {
                error: error.to_string(),
                recovery_path: Some(recovery_path),
            }
        }
    }
}

fn run_incremental_prestarted_recording(
    worker: &RecordingWorkerRequest,
    session: &mut dyn gif_from_screen_capture::CaptureSession,
    control: &mut RecordingControl,
    cancellation: &CancellationFlag,
    progress: &mut dyn gif_from_screen_workflow::WorkflowProgressSink,
    on_finalizing: impl FnOnce(),
) -> RecordingCompletion {
    if cancellation.is_cancelled() {
        let _ = session.discard();
        return RecordingCompletion::Discarded {
            cleanup_error: None,
        };
    }
    let project = match create_incremental_recording_project(worker) {
        Ok(project) => project,
        Err(error) => {
            let _ = session.discard();
            return RecordingCompletion::Failed {
                error: format!(
                    "{error}; the prepared Wayland session was discarded before recording"
                ),
                recovery_path: None,
            };
        }
    };
    let mut sink = IncrementalProjectFrameSink::new(project);
    let options = match collection_options(&worker.settings) {
        Ok(options) => options,
        Err(error) => {
            let _ = session.discard();
            return RecordingCompletion::Failed {
                error,
                recovery_path: Some(sink.root().to_path_buf()),
            };
        }
    };
    let collection = session
        .resume()
        .map_err(WorkflowError::from)
        .and_then(|()| {
            collect_prestarted_controlled_to_sink(
                session,
                &options,
                control,
                &mut sink,
                cancellation,
                progress,
            )
        });
    match collection {
        Ok(_) => {
            on_finalizing();
            let recovery_path = sink.root().to_path_buf();
            match sink.finish() {
                Ok(project) => RecordingCompletion::Completed(Box::new(project)),
                Err(error) => RecordingCompletion::Failed {
                    error: error.to_string(),
                    recovery_path: Some(recovery_path),
                },
            }
        }
        Err(WorkflowError::Discarded | WorkflowError::Cancelled) => {
            let project_path = sink.root().to_path_buf();
            drop(sink);
            RecordingCompletion::Discarded {
                cleanup_error: remove_recording_project_path(&project_path).err(),
            }
        }
        Err(error) => {
            let recovery_path = sink.root().to_path_buf();
            drop(sink);
            RecordingCompletion::Failed {
                error: error.to_string(),
                recovery_path: Some(recovery_path),
            }
        }
    }
}

fn create_incremental_recording_project(
    worker: &RecordingWorkerRequest,
) -> Result<IncrementalRecordingProject, Box<dyn std::error::Error + Send + Sync>> {
    if Path::new(worker.settings.output.trim()).try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "final GIF target appeared before recording started",
        )
        .into());
    }
    if worker.project_path.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "project directory appeared before recording started",
        )
        .into());
    }
    let created_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let created_millis = i64::try_from(created_millis)
        .map_err(|_| io::Error::other("current time does not fit the project timestamp"))?;
    IncrementalRecordingProject::create(
        &worker.project_path,
        worker.canvas,
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at: UnixTimeMs::new(created_millis),
            source_label: Some(worker.source_label.clone()),
        },
    )
    .map_err(Into::into)
}

fn collect_x11_recording(
    worker: &RecordingWorkerRequest,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &CancellationFlag,
    progress: &mut dyn gif_from_screen_workflow::WorkflowProgressSink,
) -> Result<(), WorkflowError> {
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
            _ => {
                return Err(gif_from_screen_capture::CaptureError::invalid_request(
                    "unsupported future X11 capture source kind",
                )
                .into());
            }
        }
    };
    let cadence = recording_cadence(&worker.settings)
        .map_err(gif_from_screen_capture::CaptureError::invalid_request)?;
    let mut request = CaptureRequest::new(target, cadence);
    request.cursor = CursorCaptureMode::Embedded;
    let options = collection_options(&worker.settings)
        .map_err(gif_from_screen_capture::CaptureError::invalid_request)?;
    collect_controlled_to_sink(
        &backend,
        request,
        &options,
        control,
        sink,
        cancellation,
        progress,
    )
    .map(|_| ())
}

fn collection_options(settings: &RecordingSettings) -> Result<CollectOptions, String> {
    Ok(CollectOptions {
        limit: collection_limit(settings.duration_ms),
        frame_retention: frame_retention(settings.changes_only),
        tail_frame_duration: recording_tail_frame_duration(settings)?,
        ..CollectOptions::default()
    })
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

const fn file_drop_block_reason(activity: FileDropActivity) -> Option<&'static str> {
    match activity {
        FileDropActivity::Idle => None,
        FileDropActivity::Recording => Some("the recorder or region picker is active"),
        FileDropActivity::ProjectOpen => Some("a project-open job is active"),
        FileDropActivity::GifImport => Some("a GIF import is active"),
        FileDropActivity::ImageImport => Some("an image import is active"),
        FileDropActivity::ImageSequenceImport => Some("an image-sequence import is active"),
        FileDropActivity::BlankCreation => Some("blank-project creation is active"),
        FileDropActivity::WatermarkDecode => Some("a watermark decode is active"),
        FileDropActivity::Export => Some("a GIF export is active"),
    }
}

fn prepare_file_drop_route(dropped_paths: &[Option<PathBuf>]) -> Result<FileDropRoute, String> {
    if dropped_paths.is_empty() {
        return Err("No dropped files were provided.".to_owned());
    }
    if dropped_paths.len() > 1 {
        return prepare_static_sequence_drop(dropped_paths);
    }
    let path = dropped_paths[0].as_ref().ok_or_else(|| {
        "Dropped data has no local filesystem path; save it to disk before importing.".to_owned()
    })?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not inspect dropped path {}: {error}", path.display()))?;
    let has_project_manifest = if metadata.is_dir() {
        let manifest = path.join("manifest.json");
        match fs::metadata(&manifest) {
            Ok(manifest_metadata) => manifest_metadata.is_file(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!(
                    "Could not inspect dropped project manifest {}: {error}",
                    manifest.display()
                ));
            }
        }
    } else {
        false
    };
    route_file_drop(Some(FileDropCandidate {
        path: path.clone(),
        is_directory: metadata.is_dir(),
        is_regular_file: metadata.is_file(),
        has_project_manifest,
    }))
}

fn prepare_static_sequence_drop(
    dropped_paths: &[Option<PathBuf>],
) -> Result<FileDropRoute, String> {
    if dropped_paths.len() > MAX_STATIC_SEQUENCE_FRAMES {
        return Err(format!(
            "Image-sequence drop has {} files, above the {MAX_STATIC_SEQUENCE_FRAMES}-frame limit.",
            dropped_paths.len()
        ));
    }
    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(dropped_paths.len())
        .map_err(|_| "Could not reserve the dropped image-sequence path list.".to_owned())?;
    for (index, dropped_path) in dropped_paths.iter().enumerate() {
        let path = dropped_path.as_ref().ok_or_else(|| {
            format!(
                "Dropped item {} has no local filesystem path; save every image to disk first.",
                index + 1
            )
        })?;
        let metadata = fs::metadata(path).map_err(|error| {
            format!(
                "Could not inspect dropped image {} at {}: {error}",
                index + 1,
                path.display()
            )
        })?;
        if !metadata.is_file() || !has_static_image_extension(path) {
            return Err(format!(
                "Dropped item {} ({}) is not a regular PNG, JPEG, BMP, or WebP file; multi-file drops are image sequences only.",
                index + 1,
                path.display()
            ));
        }
        inputs.push(path.clone());
    }
    Ok(FileDropRoute::ImportImageSequence(inputs))
}

fn route_file_drop(candidate: Option<FileDropCandidate>) -> Result<FileDropRoute, String> {
    let candidate = candidate.ok_or_else(|| {
        "Dropped data has no local filesystem path; save it to disk before importing.".to_owned()
    })?;
    let path = candidate.path;
    if candidate.has_project_manifest {
        return Ok(FileDropRoute::OpenProject(path));
    }
    let project_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gfsproj"));
    if candidate.is_directory {
        return if project_extension {
            Ok(FileDropRoute::OpenProject(path))
        } else {
            Err(format!(
                "Dropped directory {} is not a project: it has no manifest.json.",
                path.display()
            ))
        };
    }
    if !candidate.is_regular_file {
        return Err(format!(
            "Dropped path {} is neither a regular file nor a project directory.",
            path.display()
        ));
    }
    if project_extension {
        return Err(format!(
            "Dropped project path {} must be a directory containing manifest.json.",
            path.display()
        ));
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
    {
        Ok(FileDropRoute::ImportGif(path))
    } else if has_static_image_extension(&path) {
        Ok(FileDropRoute::ImportImage(path))
    } else {
        Err(format!(
            "Unsupported dropped file {}. Drop a .gfsproj directory, GIF, PNG, JPEG, BMP, or WebP file.",
            path.display()
        ))
    }
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
    } else if first == OsStr::new("--import-image") {
        let Some(path) = arguments.next() else {
            return StartupIntent::Invalid(
                "--import-image requires a PNG, JPG/JPEG, BMP, or WebP path".to_owned(),
            );
        };
        (StartupIntentKind::Image, PathBuf::from(path))
    } else if first.to_string_lossy().starts_with('-') {
        return StartupIntent::Invalid(format!(
            "Unknown desktop argument '{}'. Use --project PATH, --import-gif PATH, or --import-image PATH.",
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
        } else if has_static_image_extension(&path) {
            StartupIntentKind::Image
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
        StartupIntentKind::Image => StartupIntent::ImportImage(path),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupIntentKind {
    Project,
    Gif,
    Image,
}

fn has_static_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "bmp", "webp"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn main() -> eframe::Result {
    let startup_intent = parse_startup_intent(std::env::args_os().skip(1));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1040.0, 760.0])
            .with_min_inner_size([680.0, 440.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation_context| {
            appearance::configure(&creation_context.egui_ctx);
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
        num::NonZeroU64,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use eframe::egui;
    use gif_from_screen_application::{
        DEFAULT_BLANK_FRAME_LIMIT_BYTES, IncrementalRecordingProject,
        IncrementalRecordingProjectOptions, ProjectFrameSelection, ProjectGifExportReport,
    };
    use gif_from_screen_capture::{
        CaptureCadence, CaptureSourceId, CaptureSourceKind, PhysicalRect,
    };
    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize as DomainSize,
        ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, Rgba, SourceProvenance,
        UnixTimeMs,
    };
    use gif_from_screen_gif::{
        BuiltinGifEncoder, DeltaMode, DitherMode, EncodeOptions, EncodeReport, LoopBehavior,
        PaletteMode, QuantizerStrategy, RgbaFrame, Transparency,
    };
    use gif_from_screen_media::{
        LoopBehavior as ImportedLoopBehavior, StaticImageSequenceDurationPolicy,
    };
    use gif_from_screen_project::{ActiveProject, LockPolicy};
    use gif_from_screen_workflow::RecordingFrameSink;
    use tempfile::tempdir;

    use super::{
        AppView, EDITOR_PREVIEW_MAX_SIZE, EditorExportSettings, ExportDitherChoice,
        ExportFrameScope, ExportLoopChoice, ExportPaletteChoice, ExportQuantizerChoice,
        FileDropActivity, FileDropCandidate, FileDropRoute, GifFromScreenApp,
        IncrementalProjectFrameSink, MAX_COUNTDOWN_SECONDS, MAX_RECORDING_DURATION_MS,
        RecorderOverlayAction, RecorderStage, RecordingCadenceChoice, RecordingIntervalUnit,
        RecordingSettings, RecordingWorkerRequest, StartupIntent, activate_editor,
        apply_overlay_region, build_blank_project_request, build_project_export_options,
        build_static_sequence_request, can_navigate_back, collection_limit, collection_options,
        create_incremental_recording_project, default_gif_path_for_project,
        default_sequence_project_path, edited_gif_path_for_import, editor_result_notice,
        export_job_is_active, export_result_notice, file_drop_block_reason, fit_dimensions,
        frame_retention, has_static_image_extension, initial_wayland_region, landing_cards_fit,
        map_drawing_preview_point, map_preview_selection, open_project_controls_enabled,
        open_project_lock_policy, parse_startup_intent, prepare_file_drop_route,
        project_path_for_output, recording_cadence, recording_project_canvas,
        recording_tail_frame_duration, remove_completed_project, remove_recording_project_path,
        resize_nearest_rgba, resolve_export_selection, route_file_drop, should_sync_retarget,
        show_editor_scroll_area, static_sequence_duration_policy, static_sequence_loop_behavior,
        translate_source_region, validate_export_output, validate_settings,
    };
    use crate::blank_project_job::BlankProjectJobState;
    use crate::blank_project_ui::{BlankBackgroundChoice, BlankProjectUiState};
    use crate::editor_ui::{EditorUiAction, EditorUiFailure, EditorUiOperation};
    use crate::editor_workspace::EditorWorkspace;
    use crate::export_job::{ExportJobError, ExportJobState};
    use crate::import_gif_job::ImportGifJobState;
    use crate::import_static_image_job::ImportStaticImageJobState;
    use crate::import_static_sequence_job::ImportStaticSequenceJobState;
    use crate::open_project_job::OpenProjectJobState;
    use crate::static_sequence_ui::{
        StaticSequenceLoopChoice, StaticSequenceTimingChoice, StaticSequenceUiState,
    };
    use crate::watermark_decode_job::WatermarkDecodeJobState;
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
        settings.cadence = RecordingCadenceChoice::Manual;
        assert!(validate_settings(&settings).is_ok());
        settings.manual_frame_duration_ms = 0;
        assert!(validate_settings(&settings).is_err());
        settings.manual_frame_duration_ms = 100;
        settings.cadence = RecordingCadenceChoice::Periodic;
        settings.interval_count = 0;
        assert!(validate_settings(&settings).is_err());
        settings.interval_count = 1;
        assert!(validate_settings(&settings).is_ok());
        settings.cadence = RecordingCadenceChoice::FixedFps;
        settings.fps = 10;
        settings.output = "capture.mp4".into();
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn desktop_startup_arguments_route_projects_gifs_and_static_images() {
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
            parse_startup_intent(["--import-image", "/tmp/photo.data"].map(OsString::from)),
            StartupIntent::ImportImage(PathBuf::from("/tmp/photo.data"))
        );
        for extension in ["png", "JPG", "jpeg", "BMP", "webp"] {
            let path = PathBuf::from(format!("/tmp/image.{extension}"));
            assert_eq!(
                parse_startup_intent([path.clone().into_os_string()]),
                StartupIntent::ImportImage(path)
            );
        }
        assert_eq!(
            parse_startup_intent([OsString::from("/tmp/demo.gfsproj")]),
            StartupIntent::OpenProject(PathBuf::from("/tmp/demo.gfsproj"))
        );
        assert!(matches!(
            parse_startup_intent([OsString::from("--unknown")]),
            StartupIntent::Invalid(message)
                if message.contains("Unknown desktop argument")
                    && message.contains("--import-image")
        ));
        assert!(matches!(
            parse_startup_intent(["--project", "one", "two"].map(OsString::from)),
            StartupIntent::Invalid(message) if message.contains("Unexpected extra")
        ));
        assert!(matches!(
            parse_startup_intent([OsString::from("--import-image")]),
            StartupIntent::Invalid(message) if message.contains("requires")
        ));
        assert!(has_static_image_extension(Path::new("photo.JPEG")));
        assert!(!has_static_image_extension(Path::new("animation.gif")));
    }

    #[test]
    fn pure_file_drop_routing_prioritizes_project_manifests_and_supported_types() {
        let project_named_like_gif = PathBuf::from("/tmp/project.gif");
        assert_eq!(
            route_file_drop(Some(FileDropCandidate {
                path: project_named_like_gif.clone(),
                is_directory: true,
                is_regular_file: false,
                has_project_manifest: true,
            }))
            .unwrap(),
            FileDropRoute::OpenProject(project_named_like_gif)
        );
        let extension_project = PathBuf::from("/tmp/project.GFSPROJ");
        assert_eq!(
            route_file_drop(Some(FileDropCandidate {
                path: extension_project.clone(),
                is_directory: true,
                is_regular_file: false,
                has_project_manifest: false,
            }))
            .unwrap(),
            FileDropRoute::OpenProject(extension_project)
        );
        let gif = PathBuf::from("/tmp/animation.GIF");
        assert_eq!(
            route_file_drop(Some(FileDropCandidate {
                path: gif.clone(),
                is_directory: false,
                is_regular_file: true,
                has_project_manifest: false,
            }))
            .unwrap(),
            FileDropRoute::ImportGif(gif)
        );
        for extension in ["png", "JPG", "jpeg", "BMP", "webp"] {
            let image = PathBuf::from(format!("/tmp/image.{extension}"));
            assert_eq!(
                route_file_drop(Some(FileDropCandidate {
                    path: image.clone(),
                    is_directory: false,
                    is_regular_file: true,
                    has_project_manifest: false,
                }))
                .unwrap(),
                FileDropRoute::ImportImage(image)
            );
        }
        assert!(route_file_drop(None).unwrap_err().contains("no local"));
        assert!(
            route_file_drop(Some(FileDropCandidate {
                path: PathBuf::from("/tmp/movie.mp4"),
                is_directory: false,
                is_regular_file: true,
                has_project_manifest: false,
            }))
            .unwrap_err()
            .contains("Unsupported")
        );
        assert!(
            route_file_drop(Some(FileDropCandidate {
                path: PathBuf::from("/tmp/not-a-directory.gfsproj"),
                is_directory: false,
                is_regular_file: true,
                has_project_manifest: false,
            }))
            .unwrap_err()
            .contains("must be a directory")
        );
    }

    #[test]
    fn file_drop_cardinality_and_activity_rejections_are_explicit() {
        assert!(
            prepare_file_drop_route(&[None])
                .unwrap_err()
                .contains("no local filesystem path")
        );
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("second.JPG");
        let gif = directory.path().join("mixed.gif");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::write(&gif, b"gif").unwrap();
        assert_eq!(
            prepare_file_drop_route(&[Some(second.clone()), Some(first.clone())]).unwrap(),
            FileDropRoute::ImportImageSequence(vec![second.clone(), first.clone()])
        );
        assert_eq!(
            prepare_file_drop_route(&[Some(first.clone())]).unwrap(),
            FileDropRoute::ImportImage(first.clone())
        );
        assert!(
            prepare_file_drop_route(&[Some(first), Some(gif)])
                .unwrap_err()
                .contains("image sequences only")
        );
        assert!(
            prepare_file_drop_route(&[Some(second), None])
                .unwrap_err()
                .contains("no local filesystem path")
        );
        assert_eq!(file_drop_block_reason(FileDropActivity::default()), None);
        for (activity, expected) in [
            (FileDropActivity::Recording, "recorder"),
            (FileDropActivity::ProjectOpen, "project-open"),
            (FileDropActivity::GifImport, "GIF import"),
            (FileDropActivity::ImageImport, "image import"),
            (
                FileDropActivity::ImageSequenceImport,
                "image-sequence import",
            ),
            (FileDropActivity::BlankCreation, "blank-project creation"),
            (FileDropActivity::Export, "GIF export"),
        ] {
            assert!(file_drop_block_reason(activity).unwrap().contains(expected));
        }
    }

    #[test]
    fn sequence_form_maps_order_timing_loop_limits_and_metadata_into_job_request() {
        let state = StaticSequenceUiState {
            inputs: vec![PathBuf::from("second.png"), PathBuf::from("first.webp")],
            target: "ordered.gfsproj".to_owned(),
            timing: StaticSequenceTimingChoice::Uniform,
            uniform_duration_ms: 125,
            loop_choice: StaticSequenceLoopChoice::Finite,
            finite_repeats: 7,
            ..StaticSequenceUiState::default()
        };

        let request = build_static_sequence_request(&state).unwrap();
        assert_eq!(request.inputs, state.inputs);
        assert_eq!(request.target, PathBuf::from("ordered.gfsproj"));
        assert_eq!(
            request.duration_policy,
            StaticImageSequenceDurationPolicy::Uniform(NonZeroU64::new(125_000).unwrap())
        );
        assert_eq!(request.loop_behavior, ImportedLoopBehavior::Finite(7));
        assert_eq!(request.limits.max_frames, 10_000);
        assert_eq!(request.limits.max_width, 16_384);
        assert_eq!(request.limits.max_height, 16_384);
        assert_eq!(request.limits.max_total_rgba_bytes, 512 * 1024 * 1024);
        assert_eq!(request.display_names, ["second.png", "first.webp"]);
        assert_eq!(request.frame_ids.len(), 2);
        assert_ne!(request.frame_ids[0], request.frame_ids[1]);
        assert!(!request.project_id.is_nil());
        assert_eq!(
            static_sequence_duration_policy(StaticSequenceTimingChoice::PreserveDefault, 0)
                .unwrap(),
            StaticImageSequenceDurationPolicy::PreserveDecoded
        );
        assert!(static_sequence_duration_policy(StaticSequenceTimingChoice::Uniform, 0).is_err());
        assert_eq!(
            static_sequence_loop_behavior(StaticSequenceLoopChoice::Once, 0).unwrap(),
            ImportedLoopBehavior::Once
        );
        assert_eq!(
            static_sequence_loop_behavior(StaticSequenceLoopChoice::Infinite, 0).unwrap(),
            ImportedLoopBehavior::Infinite
        );
        assert!(static_sequence_loop_behavior(StaticSequenceLoopChoice::Finite, 0).is_err());
    }

    #[test]
    fn blank_form_maps_canvas_background_duration_limit_and_metadata() {
        let transparent = BlankProjectUiState {
            width: 320,
            height: 240,
            frame_duration_ms: 75,
            target: "transparent.gfsproj".to_owned(),
            ..BlankProjectUiState::default()
        };
        let request = build_blank_project_request(&transparent).unwrap();
        assert_eq!(request.target, PathBuf::from("transparent.gfsproj"));
        assert_eq!(request.canvas, DomainSize::new(320, 240).unwrap());
        assert_eq!(request.background, Rgba::TRANSPARENT);
        assert_eq!(request.frame_duration, DurationUs::new(75_000).unwrap());
        assert_eq!(request.frame_limit_bytes, DEFAULT_BLANK_FRAME_LIMIT_BYTES);
        assert!(!request.project_id.is_nil());
        assert!(!request.frame_id.is_nil());

        let solid = BlankProjectUiState {
            background_choice: BlankBackgroundChoice::Solid,
            red: 10,
            green: 20,
            blue: 30,
            alpha: 128,
            ..transparent.clone()
        };
        assert_eq!(
            build_blank_project_request(&solid).unwrap().background,
            Rgba {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 128,
            }
        );

        for invalid in [
            BlankProjectUiState {
                width: 0,
                ..transparent.clone()
            },
            BlankProjectUiState {
                width: u32::from(u16::MAX) + 1,
                ..transparent.clone()
            },
            BlankProjectUiState {
                frame_duration_ms: 0,
                ..transparent.clone()
            },
            BlankProjectUiState {
                target: "  ".to_owned(),
                ..transparent.clone()
            },
            BlankProjectUiState {
                background_choice: BlankBackgroundChoice::Solid,
                alpha: 0,
                ..transparent
            },
        ] {
            assert!(build_blank_project_request(&invalid).is_err());
        }
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
        let options = collection_options(&settings).unwrap();
        assert!(matches!(options.limit, CollectionLimit::UntilStopped));
        assert_eq!(options.frame_retention, FrameRetention::ChangesOnly);
        assert_eq!(options.tail_frame_duration, Duration::from_millis(50));
    }

    #[test]
    fn recording_cadence_and_tail_cover_fps_periodic_and_manual_modes() {
        let fixed = RecordingSettings {
            fps: 25,
            ..RecordingSettings::default()
        };
        assert!(matches!(
            recording_cadence(&fixed).unwrap(),
            CaptureCadence::FixedFps(fps) if fps.get() == 25
        ));
        assert_eq!(
            recording_tail_frame_duration(&fixed).unwrap(),
            Duration::from_millis(40)
        );

        for (unit, count, expected) in [
            (RecordingIntervalUnit::Seconds, 2, Duration::from_secs(2)),
            (RecordingIntervalUnit::Minutes, 3, Duration::from_secs(180)),
            (RecordingIntervalUnit::Hours, 4, Duration::from_secs(14_400)),
        ] {
            let periodic = RecordingSettings {
                cadence: RecordingCadenceChoice::Periodic,
                interval_count: count,
                interval_unit: unit,
                ..RecordingSettings::default()
            };
            assert_eq!(
                recording_cadence(&periodic).unwrap(),
                CaptureCadence::Interval(expected)
            );
            assert_eq!(recording_tail_frame_duration(&periodic).unwrap(), expected);
            assert_eq!(
                collection_options(&periodic).unwrap().tail_frame_duration,
                expected
            );
        }

        let manual = RecordingSettings {
            cadence: RecordingCadenceChoice::Manual,
            manual_frame_duration_ms: 250,
            fps: 0,
            ..RecordingSettings::default()
        };
        assert_eq!(recording_cadence(&manual).unwrap(), CaptureCadence::Manual);
        assert_eq!(
            recording_tail_frame_duration(&manual).unwrap(),
            Duration::from_millis(250)
        );

        for invalid in [
            RecordingSettings {
                cadence: RecordingCadenceChoice::Periodic,
                interval_count: 0,
                ..RecordingSettings::default()
            },
            RecordingSettings {
                cadence: RecordingCadenceChoice::Manual,
                manual_frame_duration_ms: 0,
                ..RecordingSettings::default()
            },
        ] {
            assert!(recording_cadence(&invalid).is_err() || collection_options(&invalid).is_err());
        }
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
            custom_palette_text: "#000000\n#FFFFFF".to_owned(),
            custom_transparency_enabled: false,
            custom_transparent_index: 0,
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
    }

    #[test]
    fn advanced_quantizer_and_dither_choices_map_to_encoder_strategies() {
        for (choice, expected) in [
            (
                ExportQuantizerChoice::MedianCut,
                QuantizerStrategy::MedianCut,
            ),
            (
                ExportQuantizerChoice::Grayscale,
                QuantizerStrategy::Grayscale,
            ),
            (ExportQuantizerChoice::Wu, QuantizerStrategy::Wu),
            (ExportQuantizerChoice::MostUsed, QuantizerStrategy::MostUsed),
            (ExportQuantizerChoice::NeuQuant, QuantizerStrategy::NeuQuant),
            (
                ExportQuantizerChoice::WebSafe216,
                QuantizerStrategy::WebSafe216,
            ),
            (
                ExportQuantizerChoice::Monochrome,
                QuantizerStrategy::Monochrome,
            ),
            (
                ExportQuantizerChoice::Windows16,
                QuantizerStrategy::Windows16,
            ),
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
            (ExportDitherChoice::Dotted, DitherMode::Dotted),
            (ExportDitherChoice::BlueNoise, DitherMode::BlueNoise),
            (
                ExportDitherChoice::InterleavedNoise,
                DitherMode::InterleavedNoise,
            ),
            (
                ExportDitherChoice::FloydSteinberg,
                DitherMode::FloydSteinberg,
            ),
            (ExportDitherChoice::Atkinson, DitherMode::Atkinson),
            (ExportDitherChoice::Burkes, DitherMode::Burkes),
            (ExportDitherChoice::SierraLite, DitherMode::SierraLite),
            (ExportDitherChoice::TwoRowSierra, DitherMode::TwoRowSierra),
            (
                ExportDitherChoice::JarvisJudiceNinke,
                DitherMode::JarvisJudiceNinke,
            ),
            (ExportDitherChoice::Stucki, DitherMode::Stucki),
            (ExportDitherChoice::StevensonArce, DitherMode::StevensonArce),
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
        let fixed_limit_error = build_project_export_options(
            &EditorExportSettings {
                max_colors: 216,
                quantizer: ExportQuantizerChoice::WebSafe216,
                ..EditorExportSettings::default()
            },
            ProjectFrameSelection::All,
        )
        .unwrap_err();
        assert!(fixed_limit_error.contains("at least 217 colors"));
    }

    #[test]
    fn custom_palette_maps_modes_limits_transparency_and_packed_rgb() {
        for (palette, expected_mode) in [
            (ExportPaletteChoice::Local, PaletteMode::LocalPerFrame),
            (ExportPaletteChoice::Global, PaletteMode::Global),
        ] {
            let custom = build_project_export_options(
                &EditorExportSettings {
                    max_colors: 3,
                    palette,
                    quantizer: ExportQuantizerChoice::Custom,
                    custom_palette_text: "#000000, #A0b1C2\n#FFFFFF".to_owned(),
                    custom_transparency_enabled: true,
                    custom_transparent_index: 1,
                    dither: ExportDitherChoice::BlueNoise,
                    ..EditorExportSettings::default()
                },
                ProjectFrameSelection::All,
            )
            .unwrap();
            let custom_palette = custom.custom_palette.unwrap();
            assert_eq!(custom.encoding.palette_mode, expected_mode);
            assert_eq!(custom.encoding.dither, DitherMode::BlueNoise);
            assert_eq!(custom.encoding.max_colors, 3);
            assert_eq!(custom_palette.color_count(), 3);
            assert_eq!(
                custom_palette.packed_rgb(),
                [0, 0, 0, 0xA0, 0xB1, 0xC2, 255, 255, 255]
            );
            assert_eq!(custom_palette.transparent_index(), Some(1));
        }

        for settings in [
            EditorExportSettings {
                max_colors: 2,
                quantizer: ExportQuantizerChoice::Custom,
                custom_palette_text: "#000000 #808080 #FFFFFF".to_owned(),
                ..EditorExportSettings::default()
            },
            EditorExportSettings {
                quantizer: ExportQuantizerChoice::Custom,
                custom_palette_text: "black, white".to_owned(),
                ..EditorExportSettings::default()
            },
            EditorExportSettings {
                quantizer: ExportQuantizerChoice::Custom,
                custom_transparency_enabled: true,
                custom_transparent_index: 2,
                ..EditorExportSettings::default()
            },
        ] {
            assert!(build_project_export_options(&settings, ProjectFrameSelection::All).is_err());
        }

        let ignored_custom_text = build_project_export_options(
            &EditorExportSettings {
                custom_palette_text: "invalid unless Custom is selected".to_owned(),
                ..EditorExportSettings::default()
            },
            ProjectFrameSelection::All,
        )
        .unwrap();
        assert!(ignored_custom_text.custom_palette.is_none());
    }

    #[test]
    fn invalid_custom_palette_is_rejected_before_background_export_starts() {
        let directory = tempdir().unwrap();
        let project_root = directory.path().join("project.gfsproj");
        let output = directory.path().join("invalid-custom.gif");
        let project = single_frame_project(&project_root);
        let mut app = GifFromScreenApp::default();
        activate_editor(&mut app.view, &mut app.editor_workspace, project).unwrap();
        app.settings.output = output.to_string_lossy().into_owned();
        app.editor_export_settings.quantizer = ExportQuantizerChoice::Custom;
        app.editor_export_settings.custom_palette_text = "#000000, not-a-color".to_owned();

        let error = app.start_editor_export().unwrap_err();

        assert!(error.contains("Invalid custom palette"));
        assert_eq!(app.export_job.state(), ExportJobState::Idle);
        assert!(!output.exists());
    }

    #[test]
    fn valid_custom_palette_runs_through_background_export() {
        let directory = tempdir().unwrap();
        let project_root = directory.path().join("project.gfsproj");
        let output = directory.path().join("custom.gif");
        let project = single_frame_project(&project_root);
        let mut app = GifFromScreenApp::default();
        activate_editor(&mut app.view, &mut app.editor_workspace, project).unwrap();
        app.settings.output = output.to_string_lossy().into_owned();
        app.editor_export_settings = EditorExportSettings {
            max_colors: 2,
            palette: ExportPaletteChoice::Global,
            quantizer: ExportQuantizerChoice::Custom,
            custom_palette_text: "#000000 #0C2238".to_owned(),
            dither: ExportDitherChoice::Dotted,
            alpha_threshold: 0,
            ..EditorExportSettings::default()
        };

        app.start_editor_export().unwrap();
        assert_eq!(app.export_job.state(), ExportJobState::Running);
        drain_export_job(&mut app);

        assert!(output.is_file());
        assert_eq!(app.export_job.state(), ExportJobState::Idle);
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Exported 1 selected frame")
        );
        let decoded = gif_from_screen_media::decode_gif(
            fs::File::open(&output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(decoded.frames().len(), 1);
        assert_eq!(decoded.frames()[0].rgba(), [12, 34, 56, 255]);
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
    fn editor_shell_surfaces_notices_and_failures_without_noisy_success_messages() {
        let operation = EditorUiOperation::RepairJournal;
        assert_eq!(
            editor_result_notice(Ok(EditorUiAction::Notice {
                operation,
                message: "Preserved journal at /tmp/rejected".to_owned(),
            })),
            Some("Preserved journal at /tmp/rejected".to_owned())
        );
        assert_eq!(
            editor_result_notice(Err(EditorUiFailure {
                operation,
                message: "repair failed".to_owned(),
            })),
            Some("Editor RepairJournal failed: repair failed".to_owned())
        );
        assert_eq!(
            editor_result_notice(Ok(EditorUiAction::Selection(operation))),
            None
        );
        assert_eq!(
            editor_result_notice(Ok(EditorUiAction::Project(operation))),
            None
        );
        assert_eq!(
            editor_result_notice(Ok(EditorUiAction::Playback { playing: true })),
            None
        );
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

    const STATIC_PNG_ALPHA: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 1, 3,
        0, 0, 0, 206, 236, 237, 201, 0, 0, 0, 6, 80, 76, 84, 69, 0, 255, 0, 255, 0, 0, 209, 155,
        74, 174, 0, 0, 0, 1, 116, 82, 78, 83, 64, 54, 58, 153, 246, 0, 0, 0, 10, 73, 68, 65, 84, 8,
        215, 99, 104, 0, 0, 0, 130, 0, 129, 221, 67, 106, 244, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ];

    fn write_import_png(path: &Path) {
        fs::write(path, STATIC_PNG_ALPHA).unwrap();
    }

    fn drain_import_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.import_gif_job.state() == ImportGifJobState::Running {
            app.receive_import_gif_messages();
            assert!(Instant::now() < deadline, "GIF import job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drain_import_image_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.import_image_job.state() == ImportStaticImageJobState::Running {
            app.receive_import_image_messages();
            assert!(Instant::now() < deadline, "image import job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drain_watermark_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.watermark_job.state() == WatermarkDecodeJobState::Running {
            app.receive_watermark_messages();
            assert!(Instant::now() < deadline, "watermark decode job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drain_import_sequence_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.import_sequence_job.state() == ImportStaticSequenceJobState::Running {
            app.receive_import_sequence_messages();
            assert!(Instant::now() < deadline, "image-sequence import timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drain_blank_project_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.blank_project_job.state() == BlankProjectJobState::Running {
            app.receive_blank_project_messages();
            assert!(Instant::now() < deadline, "blank-project job timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drain_export_job(app: &mut GifFromScreenApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.export_job.state() != ExportJobState::Idle {
            app.receive_export_messages();
            assert!(Instant::now() < deadline, "GIF export job timed out");
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
    fn decoded_watermark_is_committed_to_preview_and_can_be_undone() {
        let directory = tempdir().unwrap();
        let project_root = directory.path().join("watermark-project.gfsproj");
        let watermark_path = directory.path().join("logo.png");
        write_import_png(&watermark_path);
        let project = single_frame_project(&project_root);
        let mut app = GifFromScreenApp::default();
        activate_editor(&mut app.view, &mut app.editor_workspace, project).unwrap();
        app.watermark_ui.path = watermark_path.to_string_lossy().into_owned();
        app.watermark_ui.name = "Test logo".to_owned();
        app.watermark_ui.width = 1;
        app.watermark_ui.height = 1;
        app.pending_watermark = Some(
            crate::watermark_ui::PendingWatermark::start(
                &app.watermark_ui,
                app.editor_workspace.as_ref().unwrap(),
                &mut app.watermark_job,
            )
            .unwrap(),
        );

        // Async completion uses the form captured at Start, not the latest form.
        app.watermark_ui.name = "Changed after Start".to_owned();
        app.watermark_ui.width = u32::MAX;

        drain_watermark_job(&mut app);

        assert_eq!(app.watermark_job.state(), WatermarkDecodeJobState::Idle);
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Added 2×1 watermark")
        );
        let workspace = app.editor_workspace.as_mut().unwrap();
        assert_eq!(workspace.manifest().timeline.overlay_tracks.len(), 1);
        assert_eq!(
            workspace.manifest().timeline.overlay_tracks[0].name,
            "Test logo"
        );
        assert_eq!(workspace.manifest().assets.len(), 2);
        let rendered = crate::editor_preview::render_frame_surface(
            workspace.active_project(),
            FrameId::from_u128(7),
            1024,
        )
        .unwrap();
        assert_ne!(rendered.pixels(), &[12, 34, 56, 255]);
        assert!(workspace.undo().unwrap());
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        assert_eq!(workspace.manifest().assets.len(), 1);
    }

    #[test]
    fn decoded_watermark_rejects_changed_selection_revision_or_project_without_mutation() {
        for changed in 0..3 {
            let directory = tempdir().unwrap();
            let watermark_path = directory.path().join("logo.png");
            write_import_png(&watermark_path);
            let project = single_frame_project(&directory.path().join("original.gfsproj"));
            let mut app = GifFromScreenApp::default();
            activate_editor(&mut app.view, &mut app.editor_workspace, project).unwrap();
            app.watermark_ui.path = watermark_path.to_string_lossy().into_owned();
            app.watermark_ui.width = 1;
            app.watermark_ui.height = 1;
            app.pending_watermark = Some(
                crate::watermark_ui::PendingWatermark::start(
                    &app.watermark_ui,
                    app.editor_workspace.as_ref().unwrap(),
                    &mut app.watermark_job,
                )
                .unwrap(),
            );
            match changed {
                0 => app.editor_workspace.as_mut().unwrap().clear_selection(),
                1 => app
                    .editor_workspace
                    .as_mut()
                    .unwrap()
                    .override_selection_duration(DurationUs::new(123).unwrap())
                    .unwrap(),
                _ => {
                    let other = single_frame_project(&directory.path().join("other.gfsproj"));
                    activate_editor(&mut app.view, &mut app.editor_workspace, other).unwrap();
                }
            }
            let before = app.editor_workspace.as_ref().unwrap().manifest().clone();
            drain_watermark_job(&mut app);
            assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
            assert!(
                app.notice
                    .as_deref()
                    .unwrap()
                    .contains("changed while decoding")
            );
            assert!(app.pending_watermark.is_none());
        }
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
    fn sequence_default_target_uses_first_source_and_skips_existing_names() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("frame.png");
        fs::create_dir(directory.path().join("frame-sequence.gfsproj")).unwrap();
        fs::write(
            directory.path().join("frame-sequence-2.gfsproj"),
            b"occupied",
        )
        .unwrap();

        assert_eq!(
            default_sequence_project_path(&[first, directory.path().join("other.png")]).unwrap(),
            directory.path().join("frame-sequence-3.gfsproj")
        );
        assert!(default_sequence_project_path(&[]).is_err());
    }

    #[test]
    fn running_open_job_locks_both_back_navigation_controls() {
        assert_eq!(open_project_lock_policy(false), LockPolicy::FailIfPresent);
        assert_eq!(open_project_lock_policy(true), LockPolicy::TakeOver);
        assert!(!open_project_controls_enabled(OpenProjectJobState::Running));
        assert!(open_project_controls_enabled(OpenProjectJobState::Idle));
        assert!(open_project_controls_enabled(OpenProjectJobState::Finished));
        assert!(!can_navigate_back(
            AppView::OpenProject,
            OpenProjectJobState::Running,
            ImportGifJobState::Idle,
            ImportStaticImageJobState::Idle,
            ImportStaticSequenceJobState::Idle,
            BlankProjectJobState::Idle,
        ));
        for state in [OpenProjectJobState::Idle, OpenProjectJobState::Finished] {
            assert!(can_navigate_back(
                AppView::OpenProject,
                state,
                ImportGifJobState::Idle,
                ImportStaticImageJobState::Idle,
                ImportStaticSequenceJobState::Idle,
                BlankProjectJobState::Idle,
            ));
        }
        assert!(can_navigate_back(
            AppView::Editor,
            OpenProjectJobState::Running,
            ImportGifJobState::Running,
            ImportStaticImageJobState::Running,
            ImportStaticSequenceJobState::Running,
            BlankProjectJobState::Running,
        ));
    }

    #[test]
    fn running_import_job_locks_top_and_page_back_navigation() {
        assert!(!can_navigate_back(
            AppView::ImportGif,
            OpenProjectJobState::Idle,
            ImportGifJobState::Running,
            ImportStaticImageJobState::Idle,
            ImportStaticSequenceJobState::Idle,
            BlankProjectJobState::Idle,
        ));
        for state in [ImportGifJobState::Idle, ImportGifJobState::Finished] {
            assert!(can_navigate_back(
                AppView::ImportGif,
                OpenProjectJobState::Idle,
                state,
                ImportStaticImageJobState::Idle,
                ImportStaticSequenceJobState::Idle,
                BlankProjectJobState::Idle,
            ));
        }
        assert!(can_navigate_back(
            AppView::OpenProject,
            OpenProjectJobState::Idle,
            ImportGifJobState::Running,
            ImportStaticImageJobState::Running,
            ImportStaticSequenceJobState::Running,
            BlankProjectJobState::Running,
        ));
    }

    #[test]
    fn running_static_image_import_locks_top_and_page_back_navigation() {
        assert!(!can_navigate_back(
            AppView::ImportImage,
            OpenProjectJobState::Idle,
            ImportGifJobState::Idle,
            ImportStaticImageJobState::Running,
            ImportStaticSequenceJobState::Idle,
            BlankProjectJobState::Idle,
        ));
        for state in [
            ImportStaticImageJobState::Idle,
            ImportStaticImageJobState::Finished,
        ] {
            assert!(can_navigate_back(
                AppView::ImportImage,
                OpenProjectJobState::Idle,
                ImportGifJobState::Idle,
                state,
                ImportStaticSequenceJobState::Idle,
                BlankProjectJobState::Idle,
            ));
        }
    }

    #[test]
    fn running_image_sequence_import_locks_top_and_page_back_navigation() {
        assert!(!can_navigate_back(
            AppView::ImportImageSequence,
            OpenProjectJobState::Idle,
            ImportGifJobState::Idle,
            ImportStaticImageJobState::Idle,
            ImportStaticSequenceJobState::Running,
            BlankProjectJobState::Idle,
        ));
        for state in [
            ImportStaticSequenceJobState::Idle,
            ImportStaticSequenceJobState::Finished,
        ] {
            assert!(can_navigate_back(
                AppView::ImportImageSequence,
                OpenProjectJobState::Idle,
                ImportGifJobState::Idle,
                ImportStaticImageJobState::Idle,
                state,
                BlankProjectJobState::Idle,
            ));
        }
    }

    #[test]
    fn running_blank_project_creation_locks_top_and_page_back_navigation() {
        assert!(!can_navigate_back(
            AppView::NewBlankAnimation,
            OpenProjectJobState::Idle,
            ImportGifJobState::Idle,
            ImportStaticImageJobState::Idle,
            ImportStaticSequenceJobState::Idle,
            BlankProjectJobState::Running,
        ));
        for state in [BlankProjectJobState::Idle, BlankProjectJobState::Finished] {
            assert!(can_navigate_back(
                AppView::NewBlankAnimation,
                OpenProjectJobState::Idle,
                ImportGifJobState::Idle,
                ImportStaticImageJobState::Idle,
                ImportStaticSequenceJobState::Idle,
                state,
            ));
        }
    }

    #[test]
    fn startup_png_import_enters_editor_with_preview_and_detected_provenance() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("alpha.png");
        let output = directory.path().join("alpha.gif");
        write_import_png(&source);
        fs::write(&output, b"existing GIF").unwrap();
        let mut app = GifFromScreenApp::default();
        app.editor_ui_state.frame_number_input = "99".to_owned();
        app.editor_export_settings.overwrite = true;

        app.apply_startup_intent(StartupIntent::ImportImage(source.clone()));
        assert_eq!(app.view, AppView::ImportImage);
        assert_eq!(
            app.import_image_job.state(),
            ImportStaticImageJobState::Running
        );
        drain_import_image_job(&mut app);

        assert_eq!(
            app.import_image_job.state(),
            ImportStaticImageJobState::Idle
        );
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(Path::new(&app.settings.output), output);
        assert_eq!(fs::read(&output).unwrap(), b"existing GIF");
        assert_eq!(app.editor_ui_state.frame_number_input, "1");
        assert_eq!(app.editor_export_settings, EditorExportSettings::default());
        let (workspace_slot, preview_cache) =
            (&app.editor_workspace, &mut app.editor_preview_cache);
        let workspace = workspace_slot.as_ref().unwrap();
        assert_eq!(
            workspace.project_root(),
            directory.path().join("alpha.gfsproj")
        );
        assert!(matches!(
            workspace.manifest().source_provenance.as_slice(),
            [SourceProvenance::Imported {
                display_name,
                media_type
            }] if display_name == "alpha.png" && media_type == "image/png"
        ));
        let frame = &workspace.manifest().timeline.frames[0];
        assert_eq!(frame.duration.get(), 100_000);
        let context = egui::Context::default();
        let preview = preview_cache
            .preview(
                workspace.active_project(),
                workspace.selection().current().unwrap(),
                &context,
                [64, 64],
            )
            .unwrap();
        assert_eq!(preview.rendered_size, [2, 1]);
        assert!(app.notice.as_deref().unwrap().contains("enable Overwrite"));
    }

    #[test]
    fn malformed_static_image_resets_job_and_can_retry() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("retry.png");
        fs::write(&source, b"broken").unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ImportImage;
        app.import_image_path = source.to_string_lossy().into_owned();

        app.start_import_image().unwrap();
        drain_import_image_job(&mut app);

        assert_eq!(
            app.import_image_job.state(),
            ImportStaticImageJobState::Idle
        );
        assert_eq!(app.view, AppView::ImportImage);
        assert!(app.editor_workspace.is_none());
        assert!(app.notice.as_deref().unwrap().contains("You can correct"));
        assert!(!source.with_extension("gfsproj").exists());

        write_import_png(&source);
        app.start_import_image().unwrap();
        drain_import_image_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert!(app.editor_workspace.is_some());
    }

    #[test]
    fn successful_sequence_import_enters_editor_in_user_order_and_resets_job() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("second.png");
        let target = directory.path().join("ordered.gfsproj");
        write_import_png(&first);
        write_import_png(&second);
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ImportImageSequence;
        app.import_sequence_ui
            .replace_inputs(vec![second.clone(), first.clone()], &target);
        app.import_sequence_ui.uniform_duration_ms = 80;
        app.import_sequence_ui.loop_choice = StaticSequenceLoopChoice::Once;

        app.start_import_sequence().unwrap();
        assert_eq!(
            app.import_sequence_job.state(),
            ImportStaticSequenceJobState::Running
        );
        assert_eq!(
            app.file_drop_activity(),
            FileDropActivity::ImageSequenceImport
        );
        app.handle_dropped_paths(&[Some(first)]);
        assert_eq!(app.view, AppView::ImportImageSequence);
        assert_eq!(
            app.import_image_job.state(),
            ImportStaticImageJobState::Idle
        );
        drain_import_sequence_job(&mut app);

        assert_eq!(
            app.import_sequence_job.state(),
            ImportStaticSequenceJobState::Idle
        );
        assert_eq!(app.view, AppView::Editor);
        assert!(app.import_sequence_ui.inputs.is_empty());
        assert_eq!(
            Path::new(&app.settings.output),
            target.with_extension("gif")
        );
        let workspace = app.editor_workspace.as_ref().unwrap();
        assert_eq!(workspace.project_root(), target);
        assert_eq!(workspace.manifest().timeline.frames.len(), 2);
        assert_eq!(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [80_000, 80_000]
        );
        assert!(matches!(
            workspace.manifest().source_provenance.as_slice(),
            [
                SourceProvenance::Imported { display_name: second_name, .. },
                SourceProvenance::Imported { display_name: first_name, .. }
            ] if second_name == "second.png" && first_name == "first.png"
        ));
        assert_eq!(
            workspace.selection().current(),
            Some(workspace.manifest().timeline.frames[0].id)
        );
        assert!(app.notice.as_deref().unwrap().contains("2 ordered images"));
    }

    #[test]
    fn failed_sequence_import_keeps_form_and_can_retry() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("broken.png");
        let target = directory.path().join("retry.gfsproj");
        write_import_png(&first);
        fs::write(&second, b"broken").unwrap();
        let inputs = vec![first, second.clone()];
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ImportImageSequence;
        app.import_sequence_ui
            .replace_inputs(inputs.clone(), &target);

        app.start_import_sequence().unwrap();
        drain_import_sequence_job(&mut app);

        assert_eq!(app.view, AppView::ImportImageSequence);
        assert_eq!(app.import_sequence_ui.inputs, inputs);
        assert_eq!(
            app.import_sequence_job.state(),
            ImportStaticSequenceJobState::Idle
        );
        assert!(!target.exists());
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Adjust the ordered inputs")
        );

        write_import_png(&second);
        app.start_import_sequence().unwrap();
        drain_import_sequence_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(
            app.import_sequence_job.state(),
            ImportStaticSequenceJobState::Idle
        );
    }

    #[test]
    fn successful_blank_project_enters_editor_and_resets_one_shot_job() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("blank.gfsproj");
        let color = Rgba {
            red: 12,
            green: 34,
            blue: 56,
            alpha: 200,
        };
        let mut app = GifFromScreenApp::default();
        app.view = AppView::NewBlankAnimation;
        app.blank_project_ui = BlankProjectUiState {
            width: 2,
            height: 1,
            background_choice: BlankBackgroundChoice::Solid,
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
            frame_duration_ms: 75,
            target: target.to_string_lossy().into_owned(),
        };

        app.start_blank_project().unwrap();
        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Running);
        assert_eq!(app.file_drop_activity(), FileDropActivity::BlankCreation);
        drain_blank_project_job(&mut app);

        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Idle);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(
            Path::new(&app.settings.output),
            target.with_extension("gif")
        );
        assert_eq!(app.blank_project_ui.width, 640);
        assert_eq!(app.blank_project_ui.height, 480);
        assert_eq!(
            app.blank_project_ui.background_choice,
            BlankBackgroundChoice::Transparent
        );
        assert_eq!(app.blank_project_ui.frame_duration_ms, 100);
        assert!(app.blank_project_ui.target.ends_with(".gfsproj"));
        let workspace = app.editor_workspace.as_ref().unwrap();
        assert_eq!(workspace.project_root(), target);
        assert_eq!(
            workspace.manifest().canvas.size,
            DomainSize::new(2, 1).unwrap()
        );
        assert_eq!(
            workspace.manifest().canvas.background,
            CanvasBackground::Solid(color)
        );
        assert_eq!(workspace.manifest().timeline.frames.len(), 1);
        let frame = &workspace.manifest().timeline.frames[0];
        assert_eq!(frame.duration, DurationUs::new(75_000).unwrap());
        assert_eq!(
            workspace
                .active_project()
                .assets()
                .read(frame.asset_id)
                .unwrap(),
            [12, 34, 56, 200, 12, 34, 56, 200]
        );
        assert_eq!(workspace.selection().current(), Some(frame.id));
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Blank animation ready")
        );
    }

    #[test]
    fn blank_project_path_failure_keeps_form_and_allows_retry() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("existing.gfsproj");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::NewBlankAnimation;
        app.blank_project_ui = BlankProjectUiState {
            target: target.to_string_lossy().into_owned(),
            ..BlankProjectUiState::default()
        };
        let original_form = app.blank_project_ui.clone();

        let error = app.start_blank_project().unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Idle);
        assert_eq!(app.blank_project_ui, original_form);
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");

        fs::remove_dir_all(&target).unwrap();
        app.start_blank_project().unwrap();
        drain_blank_project_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Idle);
    }

    #[test]
    fn blank_project_worker_failure_keeps_form_and_allows_retry() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("bounded.gfsproj");
        let mut app = GifFromScreenApp::default();
        app.view = AppView::NewBlankAnimation;
        app.blank_project_ui = BlankProjectUiState {
            width: u32::from(u16::MAX),
            height: u32::from(u16::MAX),
            target: target.to_string_lossy().into_owned(),
            ..BlankProjectUiState::default()
        };
        let original_form = app.blank_project_ui.clone();

        app.start_blank_project().unwrap();
        drain_blank_project_job(&mut app);

        assert_eq!(app.view, AppView::NewBlankAnimation);
        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Idle);
        assert_eq!(app.blank_project_ui, original_form);
        assert!(!target.exists());
        assert!(app.notice.as_deref().unwrap().contains("original form"));

        app.blank_project_ui.width = 1;
        app.blank_project_ui.height = 1;
        app.start_blank_project().unwrap();
        drain_blank_project_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(app.blank_project_job.state(), BlankProjectJobState::Idle);
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
    fn dropped_manifest_project_auto_starts_open_and_enters_editor() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("manifest-priority.data");
        drop(single_frame_project(&root));
        let mut app = GifFromScreenApp::default();
        app.open_project_take_over_lock = true;

        app.handle_dropped_paths(&[Some(root.clone())]);

        assert_eq!(app.view, AppView::OpenProject);
        assert_eq!(Path::new(&app.open_project_path), root);
        assert!(!app.open_project_take_over_lock);
        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Running);
        drain_open_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(
            app.editor_workspace.as_ref().unwrap().selection().current(),
            Some(FrameId::from_u128(7))
        );
    }

    #[test]
    fn dropped_gif_and_static_image_auto_start_their_existing_import_jobs() {
        let directory = tempdir().unwrap();
        let gif = directory.path().join("dropped.gif");
        let image = directory.path().join("still.png");
        write_import_gif(&gif);
        write_import_png(&image);
        let mut app = GifFromScreenApp::default();

        app.handle_dropped_paths(&[Some(gif.clone())]);
        assert_eq!(app.view, AppView::ImportGif);
        assert_eq!(Path::new(&app.import_gif_path), gif);
        assert_eq!(app.import_gif_job.state(), ImportGifJobState::Running);
        drain_import_job(&mut app);
        assert_eq!(app.view, AppView::Editor);

        app.handle_dropped_paths(&[Some(image.clone())]);
        assert_eq!(app.view, AppView::ImportImage);
        assert_eq!(Path::new(&app.import_image_path), image);
        assert_eq!(
            app.import_image_job.state(),
            ImportStaticImageJobState::Running
        );
        drain_import_image_job(&mut app);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(
            app.editor_workspace.as_ref().unwrap().project_root(),
            directory.path().join("still.gfsproj")
        );
    }

    #[test]
    fn multi_image_drop_opens_sequence_form_in_original_order() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("second.webp");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let mut app = GifFromScreenApp::default();

        app.handle_dropped_paths(&[Some(second.clone()), Some(first.clone())]);

        assert_eq!(app.view, AppView::ImportImageSequence);
        assert_eq!(app.import_sequence_ui.inputs, [second.clone(), first]);
        assert_eq!(
            Path::new(&app.import_sequence_ui.target),
            directory.path().join("second-sequence.gfsproj")
        );
        assert_eq!(
            app.import_sequence_job.state(),
            ImportStaticSequenceJobState::Idle
        );
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("dropped-file order")
        );
    }

    #[test]
    fn rejected_drops_keep_the_active_workspace_and_routes_unchanged() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("current.gfsproj");
        let unsupported = directory.path().join("movie.mp4");
        let valid_gif = directory.path().join("blocked.gif");
        fs::write(&unsupported, b"unsupported").unwrap();
        write_import_gif(&valid_gif);
        let project = single_frame_project(&root);
        let mut app = GifFromScreenApp::default();
        activate_editor(&mut app.view, &mut app.editor_workspace, project).unwrap();

        for dropped in [
            vec![None],
            vec![
                Some(PathBuf::from("/tmp/one.gif")),
                Some(PathBuf::from("/tmp/two.gif")),
            ],
            vec![Some(unsupported)],
        ] {
            app.handle_dropped_paths(&dropped);
            assert_eq!(app.view, AppView::Editor);
            assert_eq!(app.editor_workspace.as_ref().unwrap().project_root(), root);
            assert!(app.open_project_path.is_empty());
            assert!(app.import_gif_path.is_empty());
            assert!(app.import_image_path.is_empty());
            assert!(app.import_sequence_ui.inputs.is_empty());
        }

        let _ = app.recording_countdown.start(Instant::now(), 1);
        app.handle_dropped_paths(&[Some(valid_gif)]);
        assert_eq!(app.view, AppView::Editor);
        assert_eq!(app.editor_workspace.as_ref().unwrap().project_root(), root);
        assert_eq!(app.import_gif_job.state(), ImportGifJobState::Idle);
        assert!(app.notice.as_deref().unwrap().contains("recorder"));
    }

    #[test]
    fn locked_project_requires_explicit_takeover_and_preserves_owner_lock() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("locked.gfsproj");
        let original_owner = single_frame_project(&root);
        let lock_path = root.join("project.lock");
        let original_lock = fs::read(&lock_path).unwrap();
        let mut app = GifFromScreenApp::default();
        app.view = AppView::OpenProject;
        app.open_project_path = root.to_string_lossy().into_owned();

        assert!(!app.open_project_take_over_lock);
        app.start_open_project().unwrap();
        drain_open_job(&mut app);

        assert_eq!(app.view, AppView::OpenProject);
        assert_eq!(app.open_project_job.state(), OpenProjectJobState::Idle);
        assert!(app.editor_workspace.is_none());
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("already locked"));
        assert!(notice.contains("owner: pid "));
        assert!(notice.contains("explicitly confirm"));
        assert_eq!(fs::read(&lock_path).unwrap(), original_lock);
        assert!(!root.join("project.lock.stale-1").exists());

        app.open_project_take_over_lock = true;
        app.start_open_project().unwrap();
        drain_open_job(&mut app);

        assert_eq!(app.view, AppView::Editor);
        assert!(app.editor_workspace.is_some());
        assert_eq!(
            fs::read(root.join("project.lock.stale-1")).unwrap(),
            original_lock
        );
        let replacement_lock = fs::read(&lock_path).unwrap();
        assert_ne!(replacement_lock, original_lock);
        drop(original_owner);
        assert_eq!(fs::read(&lock_path).unwrap(), replacement_lock);
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
    fn recording_canvas_is_fixed_from_region_or_selected_source_geometry() {
        let mut settings = RecordingSettings {
            region_width: 800,
            region_height: 450,
            ..RecordingSettings::default()
        };
        let source_geometry = PhysicalRect::new(-100, 20, 1_920, 1_080).unwrap();

        assert_eq!(
            recording_project_canvas(&settings, Some(source_geometry)).unwrap(),
            DomainSize::new(800, 450).unwrap()
        );
        settings.region_enabled = false;
        assert_eq!(
            recording_project_canvas(&settings, Some(source_geometry)).unwrap(),
            DomainSize::new(1_920, 1_080).unwrap()
        );
        assert!(recording_project_canvas(&settings, None).is_err());
    }

    #[test]
    fn recording_start_rechecks_output_and_project_path_after_countdown() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("race.gif");
        let project_path = directory.path().join("race.gfsproj");
        let worker = RecordingWorkerRequest {
            settings: RecordingSettings {
                output: output.to_string_lossy().into_owned(),
                ..RecordingSettings::default()
            },
            source_id: CaptureSourceId::new("test:source").unwrap(),
            source_kind: CaptureSourceKind::Monitor,
            source_label: "Test monitor".to_owned(),
            project_path: project_path.clone(),
            canvas: DomainSize::new(640, 480).unwrap(),
        };

        fs::write(&output, b"appeared during countdown").unwrap();
        let output_error = create_incremental_recording_project(&worker).unwrap_err();
        assert!(
            output_error
                .to_string()
                .contains("final GIF target appeared")
        );
        assert!(!project_path.exists());

        fs::remove_file(&output).unwrap();
        fs::create_dir(&project_path).unwrap();
        fs::write(project_path.join("sentinel"), b"keep").unwrap();
        let project_error = create_incremental_recording_project(&worker).unwrap_err();
        assert!(
            project_error
                .to_string()
                .contains("project directory appeared")
        );
        assert_eq!(fs::read(project_path.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn desktop_incremental_sink_first_frame_survives_an_unfinalized_drop() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("autosave.gfsproj");
        let writer = IncrementalRecordingProject::create(
            &root,
            DomainSize::new(1, 1).unwrap(),
            IncrementalRecordingProjectOptions {
                project_id: ProjectId::from_u128(55),
                app_version: "desktop-test".to_owned(),
                created_at: UnixTimeMs::new(1),
                source_label: Some("Test monitor".to_owned()),
            },
        )
        .unwrap();
        let mut sink = IncrementalProjectFrameSink::new(writer);
        let frame = RgbaFrame::new(1, 1, vec![12, 34, 56, 255], 100_000).unwrap();

        sink.append_provisional_frame(0, &frame).unwrap();
        drop(sink);

        let opened = ActiveProject::open(&root, LockPolicy::FailIfPresent).unwrap();
        assert!(opened.journal_recovery.is_clean());
        assert_eq!(opened.journal_recovery.replayed_records, 1);
        assert_eq!(opened.project.manifest().timeline.frames.len(), 1);
        assert_eq!(
            opened.project.manifest().timeline.frames[0].duration.get(),
            100_000
        );
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

        let unexpected = directory.path().join("do-not-delete.txt");
        fs::create_dir(&unexpected).unwrap();
        fs::write(unexpected.join("sentinel"), b"keep").unwrap();
        assert!(remove_recording_project_path(&unexpected).is_err());
        assert!(unexpected.join("sentinel").is_file());
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
    fn drawing_preview_points_map_and_clamp_to_rendered_pixels() {
        let preview = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(100.0, 50.0));
        let center =
            map_drawing_preview_point(preview, egui::pos2(70.0, 35.0), [1_000, 500]).unwrap();
        assert_eq!((center.x.get(), center.y.get()), (500, 250));
        let outside =
            map_drawing_preview_point(preview, egui::pos2(1_000.0, -100.0), [1_000, 500]).unwrap();
        assert_eq!((outside.x.get(), outside.y.get()), (999, 0));
        assert!(
            map_drawing_preview_point(preview, egui::pos2(f32::NAN, 0.0), [1_000, 500]).is_none()
        );
        assert!(map_drawing_preview_point(preview, preview.center(), [0, 500]).is_none());
    }

    #[test]
    fn wayland_initial_region_clamps_exact_inputs_to_first_frame_geometry() {
        let settings = RecordingSettings {
            region_x: 900,
            region_y: -20,
            region_width: 400,
            region_height: 800,
            ..RecordingSettings::default()
        };
        let source = gif_from_screen_capture::PhysicalSize::new(1_000, 500).unwrap();
        let region = initial_wayland_region(&settings, source);
        assert_eq!((region.origin().x, region.origin().y), (600, 0));
        assert_eq!((region.size().width(), region.size().height()), (400, 500));

        let full = initial_wayland_region(
            &RecordingSettings {
                region_enabled: false,
                ..settings
            },
            source,
        );
        assert_eq!(full, PhysicalRect::new(0, 0, 1_000, 500).unwrap());
    }

    #[test]
    fn wayland_crop_translation_preserves_size_and_clamps_at_source_edges() {
        let source = gif_from_screen_capture::PhysicalSize::new(1_000, 500).unwrap();
        let initial = PhysicalRect::new(100, 100, 300, 200).unwrap();
        assert_eq!(
            translate_source_region(initial, source, 50, -25),
            PhysicalRect::new(150, 75, 300, 200).unwrap()
        );
        assert_eq!(
            translate_source_region(initial, source, 10_000, 10_000),
            PhysicalRect::new(700, 300, 300, 200).unwrap()
        );
        assert_eq!(
            translate_source_region(initial, source, -10_000, -10_000),
            PhysicalRect::new(0, 0, 300, 200).unwrap()
        );
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
        assert!(should_sync_retarget(
            RecorderStage::Recording,
            RecorderOverlayAction::Snapshot
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
