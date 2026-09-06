#![allow(
    dead_code,
    reason = "the first editor UI is wired into the application in a follow-up change"
)]

use std::{
    fmt::Display,
    ops::Range,
    str::FromStr,
    time::{Duration, Instant},
};

use eframe::egui;
use gif_from_screen_domain::{
    BlendMode, DurationUs, EdgeWidths, Effect, FrameClip, FrameId, MAX_TRANSITION_STEPS,
    OverlayContent, PhysicalRect, PhysicalSize, Rgba, ShapeKind, SlideDirection, StrokePoint,
    TimeUs, Transition, TransitionKind,
};
use gif_from_screen_editor::{
    DuplicateDelayMode, DuplicateFrameRetention, FrameTransitionSettings,
    MAX_FRAME_EFFECT_BLUR_RADIUS, ReduceDelayMode, VirtualFilmstripError, VirtualFilmstripLayout,
    YoyoScope, parse_frame_expression,
};

use crate::editor_workspace::{EditorWorkspace, EditorWorkspaceError, OverlaySelectionAnchor};
use crate::thumbnail_cache::ThumbnailCache;

const FILMSTRIP_ITEM_WIDTH: f64 = 112.0;
const FILMSTRIP_ITEM_GAP: f64 = 8.0;
const FILMSTRIP_ITEM_HEIGHT: f32 = 78.0;
const FILMSTRIP_OVERSCAN: usize = 3;
const MAX_VISIBLE_OVERLAY_TRACKS: usize = 64;
pub(crate) const MAX_DRAWING_DRAFT_POINTS: usize = 4_096;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EditorToolTab {
    #[default]
    Frames,
    Timing,
    Transform,
    Effects,
    Overlays,
    Project,
}

impl EditorToolTab {
    const ALL: [Self; 6] = [
        Self::Frames,
        Self::Timing,
        Self::Transform,
        Self::Effects,
        Self::Overlays,
        Self::Project,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Frames => "Frames",
            Self::Timing => "Timing",
            Self::Transform => "Transform",
            Self::Effects => "Effects",
            Self::Overlays => "Overlays",
            Self::Project => "Project",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EffectChoice {
    #[default]
    Blur,
    Pixelate,
    Darken,
    Lighten,
    Border,
    Shadow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TransitionChoice {
    #[default]
    FadeToNext,
    FadeToColor,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ShapeOverlayChoice {
    Line,
    Arrow,
    #[default]
    Rectangle,
    Ellipse,
}

#[derive(Debug)]
struct ShapeOverlayUiState {
    name: String,
    kind: ShapeOverlayChoice,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    stroke_width: u16,
    stroke: Rgba,
    fill_enabled: bool,
    fill: Rgba,
    track_opacity: u8,
    blend_mode: BlendMode,
    z_index: i32,
}

impl Default for ShapeOverlayUiState {
    fn default() -> Self {
        Self {
            name: "Shape".to_owned(),
            kind: ShapeOverlayChoice::Rectangle,
            x: 0,
            y: 0,
            width: 120,
            height: 80,
            stroke_width: 2,
            stroke: Rgba {
                red: 242,
                green: 153,
                blue: 74,
                alpha: 255,
            },
            fill_enabled: false,
            fill: Rgba {
                red: 242,
                green: 153,
                blue: 74,
                alpha: 96,
            },
            track_opacity: 255,
            blend_mode: BlendMode::Normal,
            z_index: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DrawingDraftPhase {
    #[default]
    Idle,
    Capturing,
    Ready,
}

#[derive(Debug)]
pub(crate) struct DrawingOverlayDraft {
    target: Option<OverlaySelectionAnchor>,
    pub(crate) phase: DrawingDraftPhase,
    pub(crate) name: String,
    pub(crate) width: u16,
    pub(crate) color: Rgba,
    pub(crate) track_opacity: u8,
    pub(crate) blend_mode: BlendMode,
    pub(crate) z_index: i32,
    pub(crate) points: Vec<StrokePoint>,
    pub(crate) limit_reached: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OverlayTool {
    #[default]
    Text,
    Image,
    Shape,
    Drawing,
    Layers,
}

impl Default for DrawingOverlayDraft {
    fn default() -> Self {
        Self {
            target: None,
            phase: DrawingDraftPhase::Idle,
            name: "Drawing".to_owned(),
            width: 4,
            color: Rgba {
                red: 242,
                green: 153,
                blue: 74,
                alpha: 255,
            },
            track_opacity: 255,
            blend_mode: BlendMode::Normal,
            z_index: 1,
            points: Vec::new(),
            limit_reached: false,
        }
    }
}

impl DrawingOverlayDraft {
    pub(crate) fn begin_for_selection(
        &mut self,
        workspace: &EditorWorkspace,
    ) -> Result<(), EditorWorkspaceError> {
        let target = workspace.overlay_selection_anchor()?;
        self.begin();
        self.target = Some(target);
        Ok(())
    }

    /// A draft belongs to the preview and selection on which drawing began.
    pub(crate) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        if self
            .target
            .as_ref()
            .is_some_and(|target| !target.matches(workspace))
        {
            self.cancel();
        }
    }

    pub(crate) fn begin(&mut self) {
        self.points.clear();
        self.limit_reached = false;
        self.phase = DrawingDraftPhase::Capturing;
    }

    pub(crate) fn cancel(&mut self) {
        self.target = None;
        self.points.clear();
        self.limit_reached = false;
        self.phase = DrawingDraftPhase::Idle;
    }

    pub(crate) fn push_point(&mut self, point: StrokePoint) {
        if self.points.last() == Some(&point) {
            return;
        }
        if self.points.len() >= MAX_DRAWING_DRAFT_POINTS {
            self.limit_reached = true;
            self.phase = DrawingDraftPhase::Ready;
            return;
        }
        self.points.push(point);
    }

    pub(crate) fn finish_stroke(&mut self) {
        if !self.points.is_empty() {
            self.phase = DrawingDraftPhase::Ready;
        }
    }
}

/// Ephemeral editor controls and playback state retained between egui frames.
#[derive(Debug)]
pub(crate) struct EditorUiState {
    active_tool: EditorToolTab,
    /// One-based frame-number input.
    pub(crate) frame_number_input: String,
    /// Project-relative time input in integer milliseconds.
    pub(crate) time_ms_input: String,
    /// Inclusive project-relative range start in integer milliseconds.
    pub(crate) time_range_start_ms_input: String,
    /// Exclusive project-relative range end in integer milliseconds.
    pub(crate) time_range_end_ms_input: String,
    /// Microsecond input shared by override and signed adjustment actions.
    pub(crate) duration_us_input: String,
    /// Positive percentage input used to scale selected frame delays.
    pub(crate) percentage_input: String,
    /// Retain-every-N interval for frame reduction.
    pub(crate) reduce_keep_every_input: String,
    /// Delay redistribution policy for frame reduction.
    pub(crate) reduce_delay_mode: ReduceDelayMode,
    /// Source range used by the Yoyo operation.
    pub(crate) yoyo_scope: YoyoScope,
    /// Whether Yoyo clones both source endpoints onto its reverse leg.
    pub(crate) yoyo_repeat_endpoints: bool,
    /// Inclusive rendered-similarity threshold for duplicate removal.
    pub(crate) duplicate_threshold_input: String,
    /// Which frame survives a duplicate run.
    pub(crate) duplicate_retention: DuplicateFrameRetention,
    /// How duplicate-run delay is assigned to its survivor.
    pub(crate) duplicate_delay_mode: DuplicateDelayMode,
    /// Effect family currently configured by the effect controls.
    pub(crate) effect_choice: EffectChoice,
    /// One-based effect position used by Replace.
    pub(crate) effect_index_input: String,
    pub(crate) effect_region_x_input: String,
    pub(crate) effect_region_y_input: String,
    pub(crate) effect_region_width_input: String,
    pub(crate) effect_region_height_input: String,
    pub(crate) effect_blur_radius_input: String,
    pub(crate) effect_pixel_block_input: String,
    pub(crate) effect_tone_percent_input: String,
    pub(crate) effect_border_top_input: String,
    pub(crate) effect_border_right_input: String,
    pub(crate) effect_border_bottom_input: String,
    pub(crate) effect_border_left_input: String,
    pub(crate) effect_shadow_offset_x_input: String,
    pub(crate) effect_shadow_offset_y_input: String,
    pub(crate) effect_shadow_blur_input: String,
    pub(crate) effect_color_red_input: String,
    pub(crate) effect_color_green_input: String,
    pub(crate) effect_color_blue_input: String,
    pub(crate) effect_color_alpha_input: String,
    /// Comma/range expression used to replace the current frame selection.
    pub(crate) frame_expression: String,
    /// Outgoing transition family for the current frame.
    pub(crate) transition_choice: TransitionChoice,
    /// Total added transition duration in microseconds.
    pub(crate) transition_duration_us_input: String,
    /// Number of generated transition frames.
    pub(crate) transition_steps_input: String,
    pub(crate) transition_color_red_input: String,
    pub(crate) transition_color_green_input: String,
    pub(crate) transition_color_blue_input: String,
    pub(crate) transition_color_alpha_input: String,
    /// Source-coordinate crop X input in physical pixels.
    pub(crate) crop_x_input: String,
    /// Source-coordinate crop Y input in physical pixels.
    pub(crate) crop_y_input: String,
    /// Crop width input in physical pixels.
    pub(crate) crop_width_input: String,
    /// Crop height input in physical pixels.
    pub(crate) crop_height_input: String,
    /// Pre-rotation resize width input in physical pixels.
    pub(crate) resize_width_input: String,
    /// Pre-rotation resize height input in physical pixels.
    pub(crate) resize_height_input: String,
    shape_overlay: ShapeOverlayUiState,
    pub(crate) overlay_tool: OverlayTool,
    pub(crate) drawing_overlay: DrawingOverlayDraft,
    /// Monotonic playback clock when playback is active.
    pub(crate) playback: Option<PlaybackClock>,
    /// Preview-only looping; GIF export repetition is configured separately.
    pub(crate) loop_preview: bool,
    thumbnail_cache: ThumbnailCache,
    filmstrip_scroll_offset: f64,
    reveal_current_frame: bool,
}

impl Default for EditorUiState {
    fn default() -> Self {
        Self {
            active_tool: EditorToolTab::default(),
            frame_number_input: "1".into(),
            time_ms_input: "0".into(),
            time_range_start_ms_input: "0".into(),
            time_range_end_ms_input: "1000".into(),
            duration_us_input: "100000".into(),
            percentage_input: "100".into(),
            reduce_keep_every_input: "2".into(),
            reduce_delay_mode: ReduceDelayMode::DontAdjust,
            yoyo_scope: YoyoScope::Selection,
            yoyo_repeat_endpoints: false,
            duplicate_threshold_input: "100".into(),
            duplicate_retention: DuplicateFrameRetention::First,
            duplicate_delay_mode: DuplicateDelayMode::Sum,
            effect_choice: EffectChoice::Blur,
            effect_index_input: "1".into(),
            effect_region_x_input: "0".into(),
            effect_region_y_input: "0".into(),
            effect_region_width_input: "1".into(),
            effect_region_height_input: "1".into(),
            effect_blur_radius_input: "2".into(),
            effect_pixel_block_input: "8".into(),
            effect_tone_percent_input: "25".into(),
            effect_border_top_input: "1".into(),
            effect_border_right_input: "1".into(),
            effect_border_bottom_input: "1".into(),
            effect_border_left_input: "1".into(),
            effect_shadow_offset_x_input: "4".into(),
            effect_shadow_offset_y_input: "4".into(),
            effect_shadow_blur_input: "4".into(),
            effect_color_red_input: "0".into(),
            effect_color_green_input: "0".into(),
            effect_color_blue_input: "0".into(),
            effect_color_alpha_input: "255".into(),
            frame_expression: "1".into(),
            transition_choice: TransitionChoice::FadeToNext,
            transition_duration_us_input: "100000".into(),
            transition_steps_input: "5".into(),
            transition_color_red_input: "0".into(),
            transition_color_green_input: "0".into(),
            transition_color_blue_input: "0".into(),
            transition_color_alpha_input: "255".into(),
            crop_x_input: "0".into(),
            crop_y_input: "0".into(),
            crop_width_input: "1".into(),
            crop_height_input: "1".into(),
            resize_width_input: "1".into(),
            resize_height_input: "1".into(),
            shape_overlay: ShapeOverlayUiState::default(),
            overlay_tool: OverlayTool::default(),
            drawing_overlay: DrawingOverlayDraft::default(),
            playback: None,
            loop_preview: true,
            thumbnail_cache: ThumbnailCache::default(),
            filmstrip_scroll_offset: 0.0,
            reveal_current_frame: false,
        }
    }
}

impl EditorUiState {
    /// Lets the application shell place text and raster tools in the same group.
    pub(crate) fn overlays_selected(&self) -> bool {
        self.active_tool == EditorToolTab::Overlays
    }
}

/// Monotonic timing state for the frame currently being played.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlaybackClock {
    id: FrameId,
    started_at: Instant,
    duration: Duration,
}

impl PlaybackClock {
    fn new(frame_id: FrameId, frame_started_at: Instant, duration: DurationUs) -> Self {
        Self {
            id: frame_id,
            started_at: frame_started_at,
            duration: Duration::from_micros(duration.get()),
        }
    }

    fn is_due(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration
    }

    fn remaining(self, now: Instant) -> Duration {
        self.duration
            .saturating_sub(now.saturating_duration_since(self.started_at))
    }

    /// Resolve the wall-clock position without replaying missed frames. A long
    /// stall costs at most two timeline scans, even across millions of loops.
    fn advance(
        self,
        frames: &[FrameClip],
        current_index: usize,
        now: Instant,
        repeat: bool,
    ) -> Option<(FrameId, Option<Self>)> {
        frames.get(current_index)?;
        let order = (current_index..frames.len()).chain(0..if repeat { current_index } else { 0 });
        let mut elapsed = now.saturating_duration_since(self.started_at).as_nanos();
        let mut cycle_duration = 0_u128;
        for index in order.clone() {
            let frame = &frames[index];
            let duration = u128::from(frame.duration.get()) * 1_000;
            if elapsed < duration {
                return clock_at_frame_age(frame, elapsed, now);
            }
            elapsed -= duration;
            cycle_duration = cycle_duration.checked_add(duration)?;
        }
        if !repeat {
            return Some((frames.last()?.id, None));
        }

        elapsed %= cycle_duration;
        for index in order {
            let frame = &frames[index];
            let duration = u128::from(frame.duration.get()) * 1_000;
            if elapsed < duration {
                return clock_at_frame_age(frame, elapsed, now);
            }
            elapsed -= duration;
        }
        None
    }
}

fn clock_at_frame_age(
    frame: &FrameClip,
    elapsed_nanos: u128,
    now: Instant,
) -> Option<(FrameId, Option<PlaybackClock>)> {
    let elapsed = Duration::new(
        (elapsed_nanos / 1_000_000_000).try_into().ok()?,
        (elapsed_nanos % 1_000_000_000).try_into().ok()?,
    );
    let started_at = now.checked_sub(elapsed)?;
    Some((
        frame.id,
        Some(PlaybackClock::new(frame.id, started_at, frame.duration)),
    ))
}

/// User intent associated with an editor UI result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditorUiOperation {
    SelectFirst,
    SelectPrevious,
    SelectNext,
    SelectLast,
    JumpToFrame,
    JumpToTime,
    SelectAll,
    InvertSelection,
    ClearSelection,
    SelectFrame,
    ToggleFrame,
    ExtendFrameRange,
    SelectExpression,
    SelectTimeRange,
    KeepTimeRange,
    DeleteTimeRange,
    Playback,
    PlaybackStep,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectClipboardEntry,
    RemoveClipboardEntry,
    ClearClipboardHistory,
    DeleteSelection,
    DeleteBeforeSelection,
    DeleteAfterSelection,
    MoveSelectionLeft,
    MoveSelectionRight,
    ReverseSelection,
    OverrideDuration,
    AdjustDuration,
    ScaleDuration,
    ReduceFrames,
    Yoyo,
    RemoveDuplicates,
    AddEffect,
    ReplaceEffect,
    ClearEffects,
    AddShapeOverlay,
    AddDrawingOverlay,
    RemoveOverlayTrack,
    SetTransition,
    DeleteTransition,
    SaveCheckpoint,
    SaveAndCompact,
    RepairJournal,
    Statistics,
    ApplyCrop,
    ClearCrop,
    Resize,
    ClearOutputSize,
    RotateLeft,
    RotateRight,
    FlipHorizontal,
    FlipVertical,
    FilmstripLayout,
}

/// Successful state change emitted by [`show_editor_ui`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EditorUiAction {
    Selection(EditorUiOperation),
    Project(EditorUiOperation),
    Playback {
        playing: bool,
    },
    Clipboard {
        operation: EditorUiOperation,
        frames: usize,
    },
    Notice {
        operation: EditorUiOperation,
        message: String,
    },
}

/// Recoverable editor UI failure returned to the application shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditorUiFailure {
    pub(crate) operation: EditorUiOperation,
    pub(crate) message: String,
}

/// One successful action or recoverable failure produced during an egui frame.
pub(crate) type EditorUiResult = Result<EditorUiAction, EditorUiFailure>;

/// Compatibility layout for hosts that place chrome and tools in one column.
pub(crate) fn show_editor_ui(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
) -> Vec<EditorUiResult> {
    let mut results = show_editor_chrome(ui, workspace, state);
    results.extend(show_editor_tool_panel(ui, workspace, state));
    results
}

/// Draws persistent navigation, the virtual filmstrip, tool tabs and history.
/// The shell can place the preview next to the selected tool panel below this chrome.
pub(crate) fn show_editor_chrome(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
) -> Vec<EditorUiResult> {
    let now = Instant::now();
    let mut results = Vec::new();
    advance_playback(ui.ctx(), workspace, state, now, &mut results);

    show_editor_summary(ui, workspace);
    show_navigation_toolbar(ui, workspace, state, now, &mut results);
    show_virtual_filmstrip(ui, workspace, state, now, &mut results);
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        for tool in EditorToolTab::ALL {
            ui.selectable_value(&mut state.active_tool, tool, tool.label());
        }
        ui.separator();
        show_history_buttons(ui, workspace, state, now, &mut results);
    });
    ui.separator();
    schedule_playback_repaint(ui.ctx(), state, now);
    results
}

/// Draws only the selected editing tools; the host owns sizing and scrolling.
pub(crate) fn show_editor_tool_panel(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
) -> Vec<EditorUiResult> {
    let now = Instant::now();
    let mut results = Vec::new();
    show_active_tool(ui, workspace, state, now, &mut results);
    schedule_playback_repaint(ui.ctx(), state, now);
    results
}

fn show_active_tool(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    match state.active_tool {
        EditorToolTab::Frames => {
            show_selection_toolbar(ui, workspace, state, now, results);
            show_edit_toolbar(ui, workspace, state, now, results);
            show_clipboard_history(ui, workspace, results);
        }
        EditorToolTab::Timing => {
            show_delay_toolbar(ui, workspace, state, now, results);
            show_time_range_toolbar(ui, workspace, state, now, results);
            show_advanced_timing_toolbar(ui, workspace, state, now, results);
            show_transition_toolbar(ui, workspace, state, now, results);
        }
        EditorToolTab::Transform => show_transform_toolbar(ui, workspace, state, now, results),
        EditorToolTab::Effects => show_effect_toolbar(ui, workspace, state, now, results),
        EditorToolTab::Overlays => {
            ui.horizontal_wrapped(|ui| {
                for (tool, label) in [
                    (OverlayTool::Text, "Text & titles"),
                    (OverlayTool::Image, "Image"),
                    (OverlayTool::Shape, "Shape"),
                    (OverlayTool::Drawing, "Draw"),
                    (OverlayTool::Layers, "Layers"),
                ] {
                    ui.selectable_value(&mut state.overlay_tool, tool, label);
                }
            });
            match state.overlay_tool {
                OverlayTool::Shape => {
                    show_shape_overlay_toolbar(ui, workspace, state, now, results);
                }
                OverlayTool::Drawing => {
                    show_drawing_overlay_controls(ui, workspace, state, now, results);
                }
                OverlayTool::Layers => show_overlay_track_list(ui, workspace, state, now, results),
                OverlayTool::Text | OverlayTool::Image => {}
            }
        }
        EditorToolTab::Project => {
            show_project_storage_toolbar(ui, workspace, results);
            show_editor_statistics(ui, workspace, results);
        }
    }
}

fn show_editor_summary(ui: &mut egui::Ui, workspace: &EditorWorkspace) {
    ui.horizontal_wrapped(|ui| {
        ui.heading("Editor");
        ui.label(format!(
            "{} frames",
            workspace.manifest().timeline.frames.len()
        ));
        ui.label(format!("{} selected", workspace.selection().len()));
        ui.label(format!(
            "Clipboard: {} frame(s) in {} snapshot(s)",
            workspace.clipboard_len(),
            workspace.clipboard_history_len()
        ));
        if workspace.is_dirty() {
            ui.strong("Journaled · checkpoint pending");
        }
        if !workspace.asset_issues().is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{} asset issue(s)", workspace.asset_issues().len()),
            );
        }
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClipboardHistoryUiAction {
    Select(gif_from_screen_editor::FrameClipboardEntryId),
    Remove(gif_from_screen_editor::FrameClipboardEntryId),
    Clear,
}

fn show_clipboard_history(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    results: &mut Vec<EditorUiResult>,
) {
    let entry_count = workspace.clipboard_history_len();
    egui::CollapsingHeader::new(format!("Clipboard history ({entry_count})"))
        .id_salt("editor-clipboard-history")
        .show(ui, |ui| {
            if entry_count == 0 {
                ui.weak("Copy or cut frames to create a session-local snapshot.");
                return;
            }

            let selected = workspace.selected_clipboard_id();
            let entries = workspace
                .clipboard_history_entries()
                .rev()
                .map(|entry| (entry.id(), entry.frame_count(), entry.total_duration_us()))
                .collect::<Vec<_>>();
            let mut action = None;
            for (id, frames, duration_us) in entries {
                ui.horizontal(|ui| {
                    let duration = duration_us
                        .map_or_else(|| "duration overflow".to_owned(), format_duration_us);
                    if ui
                        .selectable_label(
                            selected == Some(id),
                            format!("#{} · {frames} frame(s) · {duration}", id.get()),
                        )
                        .on_hover_text("Use this snapshot for Paste")
                        .clicked()
                    {
                        action = Some(ClipboardHistoryUiAction::Select(id));
                    }
                    if ui.small_button("Remove").clicked() {
                        action = Some(ClipboardHistoryUiAction::Remove(id));
                    }
                });
            }
            if ui.button("Clear clipboard history").clicked() {
                action = Some(ClipboardHistoryUiAction::Clear);
            }

            match action {
                Some(ClipboardHistoryUiAction::Select(id)) => {
                    if workspace.select_clipboard_entry(id) {
                        let frames = workspace.clipboard_len();
                        results.push(Ok(EditorUiAction::Clipboard {
                            operation: EditorUiOperation::SelectClipboardEntry,
                            frames,
                        }));
                    } else {
                        push_failure(
                            results,
                            EditorUiOperation::SelectClipboardEntry,
                            "Clipboard history changed before the entry could be selected.",
                        );
                    }
                }
                Some(ClipboardHistoryUiAction::Remove(id)) => {
                    match workspace.remove_clipboard_entry(id) {
                        Some(frames) => results.push(Ok(EditorUiAction::Clipboard {
                            operation: EditorUiOperation::RemoveClipboardEntry,
                            frames,
                        })),
                        None => push_failure(
                            results,
                            EditorUiOperation::RemoveClipboardEntry,
                            "Clipboard history changed before the entry could be removed.",
                        ),
                    }
                }
                Some(ClipboardHistoryUiAction::Clear) => {
                    workspace.clear_clipboard_history();
                    push_notice(
                        results,
                        EditorUiOperation::ClearClipboardHistory,
                        "Clipboard history cleared.",
                    );
                }
                None => {}
            }
        });
}

fn show_project_storage_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Save checkpoint").clicked() {
            match workspace.checkpoint() {
                Ok(()) => push_notice(
                    results,
                    EditorUiOperation::SaveCheckpoint,
                    "Project manifest checkpoint saved.",
                ),
                Err(error) => push_failure(results, EditorUiOperation::SaveCheckpoint, error),
            }
        }
        if ui.button("Save & compact").clicked() {
            match workspace.checkpoint_and_compact() {
                Ok(()) => push_notice(
                    results,
                    EditorUiOperation::SaveAndCompact,
                    "Project checkpoint saved and journal compacted.",
                ),
                Err(error) => push_failure(results, EditorUiOperation::SaveAndCompact, error),
            }
        }
        if workspace.journal_requires_repair() && ui.button("Repair journal").clicked() {
            match workspace.repair_journal() {
                Ok(preserved) => push_notice(
                    results,
                    EditorUiOperation::RepairJournal,
                    repair_journal_notice(preserved.as_deref()),
                ),
                Err(error) => push_failure(results, EditorUiOperation::RepairJournal, error),
            }
        }
    });
}

fn show_editor_statistics(
    ui: &mut egui::Ui,
    workspace: &EditorWorkspace,
    results: &mut Vec<EditorUiResult>,
) {
    egui::CollapsingHeader::new("Statistics")
        .default_open(false)
        .show(ui, |ui| match workspace.statistics() {
            Ok(statistics) => {
                egui::Grid::new("editor_statistics_grid")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        statistic_row(ui, "Frames", statistics.frame_count.to_string());
                        statistic_row(
                            ui,
                            "Selected frames",
                            statistics.selected_frame_count.to_string(),
                        );
                        statistic_row(
                            ui,
                            "Canvas",
                            format!(
                                "{} × {}",
                                statistics.canvas.width.get(),
                                statistics.canvas.height.get()
                            ),
                        );
                        statistic_row(
                            ui,
                            "Total duration",
                            format_duration_us(statistics.total_duration_us),
                        );
                        statistic_row(
                            ui,
                            "Selected duration",
                            format_duration_us(statistics.selection_duration_us),
                        );
                        statistic_row(
                            ui,
                            "Minimum delay",
                            format_optional_duration(statistics.minimum_delay_us),
                        );
                        statistic_row(
                            ui,
                            "Maximum delay",
                            format_optional_duration(statistics.maximum_delay_us),
                        );
                        statistic_row(
                            ui,
                            "Average delay",
                            format_optional_duration(statistics.average_delay_us),
                        );
                        statistic_row(
                            ui,
                            "Unique assets",
                            statistics.unique_asset_count.to_string(),
                        );
                        statistic_row(
                            ui,
                            "Asset descriptor bytes",
                            statistics.asset_descriptor_bytes.to_string(),
                        );
                        let current = statistics.current_frame.map_or_else(
                            || "None".to_owned(),
                            |current| {
                                format!(
                                    "#{} · start {} · delay {}",
                                    current.frame_number,
                                    format_duration_us(current.start_us),
                                    format_duration_us(current.duration_us)
                                )
                            },
                        );
                        statistic_row(ui, "Current frame", current);
                    });
            }
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                push_failure(results, EditorUiOperation::Statistics, error);
            }
        });
}

fn statistic_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.monospace(value);
    ui.end_row();
}

fn format_duration_us(duration_us: u64) -> String {
    format!(
        "{}.{:06} s",
        duration_us / 1_000_000,
        duration_us % 1_000_000
    )
}

fn format_optional_duration(duration_us: Option<u64>) -> String {
    duration_us.map_or_else(|| "None".to_owned(), format_duration_us)
}

fn repair_journal_notice(preserved: Option<&std::path::Path>) -> String {
    preserved.map_or_else(
        || "Journal is already clean; no repair was needed.".to_owned(),
        |path| format!("Rejected journal preserved at {}.", path.display()),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "keeping one wrapped navigation toolbar together mirrors its visual grouping"
)]
fn show_navigation_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let has_frames = !workspace.manifest().timeline.frames.is_empty();
    ui.horizontal_wrapped(|ui| {
        ui.label("Navigate");
        if ui
            .add_enabled(has_frames, egui::Button::new("First"))
            .clicked()
        {
            let result = workspace.select_first();
            record_selection_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::SelectFirst,
                result,
            );
        }
        if ui
            .add_enabled(has_frames, egui::Button::new("Previous"))
            .clicked()
        {
            let result = workspace.select_previous();
            record_selection_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::SelectPrevious,
                result,
            );
        }
        let play_label = if state.playback.is_some() {
            "Pause"
        } else {
            "Play"
        };
        if ui
            .add_enabled(has_frames, egui::Button::new(play_label))
            .clicked()
        {
            toggle_playback(ui.ctx(), workspace, state, now, results);
        }
        if ui
            .add_enabled(has_frames, egui::Button::new("Next"))
            .clicked()
        {
            let result = workspace.select_next();
            record_selection_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::SelectNext,
                result,
            );
        }
        if ui
            .add_enabled(has_frames, egui::Button::new("Last"))
            .clicked()
        {
            let result = workspace.select_last();
            record_selection_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::SelectLast,
                result,
            );
        }
        ui.checkbox(&mut state.loop_preview, "Loop preview")
            .on_hover_text(
                "Repeat editor playback. GIF export repetition is configured separately.",
            );

        ui.separator();
        ui.label("Frame");
        ui.add(egui::TextEdit::singleline(&mut state.frame_number_input).desired_width(54.0));
        if ui.button("Go").clicked() {
            match parse_input::<usize>(&state.frame_number_input, "frame number") {
                Ok(frame_number) => {
                    let result = workspace.select_frame_number(frame_number);
                    record_selection_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::JumpToFrame,
                        result,
                    );
                }
                Err(message) => push_failure(results, EditorUiOperation::JumpToFrame, message),
            }
        }
        ui.label("Time ms");
        ui.add(egui::TextEdit::singleline(&mut state.time_ms_input).desired_width(72.0));
        if ui.button("Go to time").clicked() {
            match parse_time_ms(&state.time_ms_input) {
                Ok(time) => {
                    let result = workspace.select_time(time);
                    record_selection_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::JumpToTime,
                        result,
                    );
                }
                Err(message) => push_failure(results, EditorUiOperation::JumpToTime, message),
            }
        }
    });
}

fn show_selection_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Select");
        if ui.button("All").clicked() {
            workspace.select_all();
            selection_succeeded(workspace, state, now, results, EditorUiOperation::SelectAll);
        }
        if ui.button("Invert").clicked() {
            workspace.invert_selection();
            selection_succeeded(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::InvertSelection,
            );
        }
        if ui.button("Clear").clicked() {
            workspace.clear_selection();
            selection_succeeded(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::ClearSelection,
            );
        }
        ui.separator();
        ui.label("Expression");
        ui.add(egui::TextEdit::singleline(&mut state.frame_expression).desired_width(180.0));
        if ui.button("Apply selection").clicked() {
            apply_frame_expression(workspace, state, now, results);
        }
    });
}

fn show_time_range_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Time range [start, end) ms");
            ui.label("Start");
            ui.add(
                egui::TextEdit::singleline(&mut state.time_range_start_ms_input)
                    .desired_width(84.0),
            );
            ui.label("End");
            ui.add(
                egui::TextEdit::singleline(&mut state.time_range_end_ms_input).desired_width(84.0),
            );
            if ui.button("Select range").clicked() {
                match parse_time_range(state) {
                    Ok((start, end)) => {
                        let result = workspace.select_time_range(start, end);
                        record_selection_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::SelectTimeRange,
                            result,
                        );
                    }
                    Err(message) => {
                        push_failure(results, EditorUiOperation::SelectTimeRange, message);
                    }
                }
            }
            if ui.button("Keep range").clicked() {
                match parse_time_range(state) {
                    Ok((start, end)) => {
                        let result = workspace.keep_time_range(start, end);
                        record_project_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::KeepTimeRange,
                            result,
                        );
                    }
                    Err(message) => {
                        push_failure(results, EditorUiOperation::KeepTimeRange, message);
                    }
                }
            }
            if ui.button("Delete range").clicked() {
                match parse_time_range(state) {
                    Ok((start, end)) => {
                        let result = workspace.delete_time_range(start, end);
                        record_project_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::DeleteTimeRange,
                            result,
                        );
                    }
                    Err(message) => {
                        push_failure(results, EditorUiOperation::DeleteTimeRange, message);
                    }
                }
            }
        });
    });
}

fn show_history_buttons(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    if ui
        .add_enabled(workspace.can_undo(), egui::Button::new("Undo"))
        .clicked()
    {
        record_history_result(
            workspace,
            state,
            now,
            results,
            EditorUiOperation::Undo,
            true,
        );
    }
    if ui
        .add_enabled(workspace.can_redo(), egui::Button::new("Redo"))
        .clicked()
    {
        record_history_result(
            workspace,
            state,
            now,
            results,
            EditorUiOperation::Redo,
            false,
        );
    }
}

fn show_edit_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Cut").clicked() {
            let result = workspace.cut_selection().map(|_| ());
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::Cut,
                result,
            );
        }
        if ui.button("Copy").clicked() {
            match workspace.copy_selection() {
                Ok(frames) => results.push(Ok(EditorUiAction::Clipboard {
                    operation: EditorUiOperation::Copy,
                    frames,
                })),
                Err(error) => push_failure(results, EditorUiOperation::Copy, error),
            }
        }
        if ui.button("Paste").clicked() {
            let result = workspace.paste_after_current().map(|_| ());
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::Paste,
                result,
            );
        }
        if ui.button("Delete").clicked() {
            let result = workspace.delete_selection();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::DeleteSelection,
                result,
            );
        }
        if ui.button("Delete before").clicked() {
            let result = workspace.delete_before_selection();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::DeleteBeforeSelection,
                result,
            );
        }
        if ui.button("Delete after").clicked() {
            let result = workspace.delete_after_selection();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::DeleteAfterSelection,
                result,
            );
        }
        if ui.button("Move left").clicked() {
            let result = workspace.move_selection_left();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::MoveSelectionLeft,
                result,
            );
        }
        if ui.button("Move right").clicked() {
            let result = workspace.move_selection_right();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::MoveSelectionRight,
                result,
            );
        }
        if ui.button("Reverse").clicked() {
            let result = workspace.reverse_selection();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::ReverseSelection,
                result,
            );
        }
    });
}

fn show_delay_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Delay µs");
        ui.add(egui::TextEdit::singleline(&mut state.duration_us_input).desired_width(92.0));
        if ui.button("Override").clicked() {
            match parse_duration_us(&state.duration_us_input) {
                Ok(duration) => {
                    let result = workspace.override_selection_duration(duration);
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::OverrideDuration,
                        result,
                    );
                }
                Err(message) => {
                    push_failure(results, EditorUiOperation::OverrideDuration, message);
                }
            }
        }
        if ui.button("Adjust signed").clicked() {
            match parse_input::<i64>(&state.duration_us_input, "delay adjustment") {
                Ok(delta) => {
                    let result = workspace.adjust_selection_duration(delta);
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::AdjustDuration,
                        result,
                    );
                }
                Err(message) => {
                    push_failure(results, EditorUiOperation::AdjustDuration, message);
                }
            }
        }
        ui.separator();
        ui.label("Percent");
        ui.add(egui::TextEdit::singleline(&mut state.percentage_input).desired_width(60.0));
        if ui.button("Scale").clicked() {
            match parse_input::<u32>(&state.percentage_input, "duration percentage") {
                Ok(percent) => {
                    let result = workspace.scale_selection_duration(percent);
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::ScaleDuration,
                        result,
                    );
                }
                Err(message) => {
                    push_failure(results, EditorUiOperation::ScaleDuration, message);
                }
            }
        }
    });
}

fn show_advanced_timing_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.group(|ui| {
        ui.strong("Advanced timing");
        ui.horizontal_wrapped(|ui| {
            ui.label("Reduce: keep every");
            ui.add(
                egui::TextEdit::singleline(&mut state.reduce_keep_every_input).desired_width(54.0),
            );
            egui::ComboBox::from_id_salt("reduce_delay_mode")
                .selected_text(reduce_delay_label(state.reduce_delay_mode))
                .show_ui(ui, |ui| {
                    for mode in [
                        ReduceDelayMode::DontAdjust,
                        ReduceDelayMode::Previous,
                        ReduceDelayMode::Evenly,
                    ] {
                        ui.selectable_value(
                            &mut state.reduce_delay_mode,
                            mode,
                            reduce_delay_label(mode),
                        );
                    }
                });
            if ui.button("Reduce frames").clicked() {
                match parse_keep_every(&state.reduce_keep_every_input) {
                    Ok(keep_every) => {
                        let result =
                            workspace.reduce_selection(keep_every, state.reduce_delay_mode);
                        record_project_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::ReduceFrames,
                            result,
                        );
                    }
                    Err(message) => {
                        push_failure(results, EditorUiOperation::ReduceFrames, message);
                    }
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Yoyo source");
            egui::ComboBox::from_id_salt("yoyo_scope")
                .selected_text(yoyo_scope_label(state.yoyo_scope))
                .show_ui(ui, |ui| {
                    for scope in [YoyoScope::Selection, YoyoScope::EntireTimeline] {
                        ui.selectable_value(&mut state.yoyo_scope, scope, yoyo_scope_label(scope));
                    }
                });
            ui.checkbox(&mut state.yoyo_repeat_endpoints, "Repeat endpoints");
            if ui.button("Create Yoyo").clicked() {
                let result = workspace.yoyo(state.yoyo_scope, state.yoyo_repeat_endpoints);
                record_project_result(
                    workspace,
                    state,
                    now,
                    results,
                    EditorUiOperation::Yoyo,
                    result,
                );
            }
        });
        show_duplicate_controls(ui, workspace, state, now, results);
    });
}

fn show_duplicate_controls(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Rendered duplicates ≥");
        ui.add(
            egui::TextEdit::singleline(&mut state.duplicate_threshold_input).desired_width(48.0),
        );
        ui.label("%");
        egui::ComboBox::from_id_salt("duplicate_retention")
            .selected_text(duplicate_retention_label(state.duplicate_retention))
            .show_ui(ui, |ui| {
                for retention in [
                    DuplicateFrameRetention::First,
                    DuplicateFrameRetention::Last,
                ] {
                    ui.selectable_value(
                        &mut state.duplicate_retention,
                        retention,
                        duplicate_retention_label(retention),
                    );
                }
            });
        egui::ComboBox::from_id_salt("duplicate_delay_mode")
            .selected_text(duplicate_delay_label(state.duplicate_delay_mode))
            .show_ui(ui, |ui| {
                for mode in [
                    DuplicateDelayMode::Keep,
                    DuplicateDelayMode::Sum,
                    DuplicateDelayMode::Average,
                ] {
                    ui.selectable_value(
                        &mut state.duplicate_delay_mode,
                        mode,
                        duplicate_delay_label(mode),
                    );
                }
            });
        if ui.button("Remove duplicates").clicked() {
            match parse_similarity_threshold(&state.duplicate_threshold_input) {
                Ok(threshold) => {
                    let result = workspace.remove_duplicate_selection(
                        threshold,
                        state.duplicate_retention,
                        state.duplicate_delay_mode,
                    );
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::RemoveDuplicates,
                        result,
                    );
                }
                Err(message) => {
                    push_failure(results, EditorUiOperation::RemoveDuplicates, message);
                }
            }
        }
    });
    ui.weak("Synchronous scan is limited to 256 selected frames and bounded render surfaces.");
}

const fn reduce_delay_label(mode: ReduceDelayMode) -> &'static str {
    match mode {
        ReduceDelayMode::DontAdjust => "Shorten timing",
        ReduceDelayMode::Previous => "Add delay to previous",
        ReduceDelayMode::Evenly => "Distribute delay evenly",
    }
}

const fn yoyo_scope_label(scope: YoyoScope) -> &'static str {
    match scope {
        YoyoScope::Selection => "Selection",
        YoyoScope::EntireTimeline => "Entire timeline",
    }
}

fn parse_keep_every(input: &str) -> Result<usize, String> {
    let keep_every = parse_input::<usize>(input, "reduce interval")?;
    if keep_every < 2 {
        return Err("reduce interval must be at least 2".to_owned());
    }
    Ok(keep_every)
}

const fn duplicate_retention_label(retention: DuplicateFrameRetention) -> &'static str {
    match retention {
        DuplicateFrameRetention::First => "Keep first",
        DuplicateFrameRetention::Last => "Keep last",
    }
}

const fn duplicate_delay_label(mode: DuplicateDelayMode) -> &'static str {
    match mode {
        DuplicateDelayMode::Keep => "Keep delay",
        DuplicateDelayMode::Sum => "Sum delay",
        DuplicateDelayMode::Average => "Average delay",
    }
}

fn parse_similarity_threshold(input: &str) -> Result<u8, String> {
    let threshold = parse_input::<u8>(input, "duplicate similarity threshold")?;
    if threshold > 100 {
        return Err("duplicate similarity threshold must be between 0 and 100".to_owned());
    }
    Ok(threshold)
}

fn show_transition_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    egui::CollapsingHeader::new("Transitions")
        .default_open(false)
        .show(ui, |ui| {
            if let Some(transition) = workspace.current_transition() {
                ui.label(format_transition_summary(transition));
            } else {
                ui.weak("The current frame has no outgoing transition.");
            }
            show_transition_inputs(ui, state);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Create / replace").clicked() {
                    match build_transition_settings(state) {
                        Ok(settings) => {
                            let result = workspace.set_current_transition(settings);
                            record_project_result(
                                workspace,
                                state,
                                now,
                                results,
                                EditorUiOperation::SetTransition,
                                result,
                            );
                        }
                        Err(message) => {
                            push_failure(results, EditorUiOperation::SetTransition, message);
                        }
                    }
                }
                if ui.button("Delete current pair transition").clicked() {
                    let result = workspace.remove_current_transition();
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::DeleteTransition,
                        result,
                    );
                }
            });
            ui.weak(format!(
                "Duration is added to the timeline; steps must be 1..={MAX_TRANSITION_STEPS}."
            ));
        });
}

fn show_transition_inputs(ui: &mut egui::Ui, state: &mut EditorUiState) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Type");
        egui::ComboBox::from_id_salt("current_frame_transition_kind")
            .selected_text(transition_choice_label(state.transition_choice))
            .show_ui(ui, |ui| {
                for choice in [
                    TransitionChoice::FadeToNext,
                    TransitionChoice::FadeToColor,
                    TransitionChoice::SlideLeft,
                    TransitionChoice::SlideRight,
                    TransitionChoice::SlideUp,
                    TransitionChoice::SlideDown,
                ] {
                    ui.selectable_value(
                        &mut state.transition_choice,
                        choice,
                        transition_choice_label(choice),
                    );
                }
            });
        ui.label("Total µs");
        ui.add(
            egui::TextEdit::singleline(&mut state.transition_duration_us_input).desired_width(92.0),
        );
        ui.label("Steps");
        compact_input(ui, &mut state.transition_steps_input);
    });
    if state.transition_choice == TransitionChoice::FadeToColor {
        ui.horizontal_wrapped(|ui| {
            ui.label("Fade RGBA");
            compact_input(ui, &mut state.transition_color_red_input);
            compact_input(ui, &mut state.transition_color_green_input);
            compact_input(ui, &mut state.transition_color_blue_input);
            compact_input(ui, &mut state.transition_color_alpha_input);
        });
    }
}

fn build_transition_settings(state: &EditorUiState) -> Result<FrameTransitionSettings, String> {
    let duration_us = parse_input::<u64>(
        &state.transition_duration_us_input,
        "transition duration in microseconds",
    )?;
    let duration = DurationUs::new(duration_us)
        .ok_or_else(|| "transition duration must be positive".to_owned())?;
    let steps = parse_input::<u16>(&state.transition_steps_input, "transition steps")?;
    if !(1..=MAX_TRANSITION_STEPS).contains(&steps) {
        return Err(format!(
            "transition steps must be between 1 and {MAX_TRANSITION_STEPS}"
        ));
    }
    if duration.get() < u64::from(steps) {
        return Err("transition duration must provide at least 1 µs per step".to_owned());
    }
    let kind = match state.transition_choice {
        TransitionChoice::FadeToNext => TransitionKind::FadeToNext,
        TransitionChoice::FadeToColor => TransitionKind::FadeToColor {
            color: parse_transition_color(state)?,
        },
        TransitionChoice::SlideLeft => TransitionKind::Slide {
            direction: SlideDirection::Left,
        },
        TransitionChoice::SlideRight => TransitionKind::Slide {
            direction: SlideDirection::Right,
        },
        TransitionChoice::SlideUp => TransitionKind::Slide {
            direction: SlideDirection::Up,
        },
        TransitionChoice::SlideDown => TransitionKind::Slide {
            direction: SlideDirection::Down,
        },
    };
    Ok(FrameTransitionSettings {
        duration,
        steps,
        kind,
    })
}

fn parse_transition_color(state: &EditorUiState) -> Result<Rgba, String> {
    Ok(Rgba {
        red: parse_input::<u8>(&state.transition_color_red_input, "transition red")?,
        green: parse_input::<u8>(&state.transition_color_green_input, "transition green")?,
        blue: parse_input::<u8>(&state.transition_color_blue_input, "transition blue")?,
        alpha: parse_input::<u8>(&state.transition_color_alpha_input, "transition alpha")?,
    })
}

const fn transition_choice_label(choice: TransitionChoice) -> &'static str {
    match choice {
        TransitionChoice::FadeToNext => "Fade to next",
        TransitionChoice::FadeToColor => "Fade to RGBA",
        TransitionChoice::SlideLeft => "Slide left",
        TransitionChoice::SlideRight => "Slide right",
        TransitionChoice::SlideUp => "Slide up",
        TransitionChoice::SlideDown => "Slide down",
    }
}

fn format_transition_summary(transition: &Transition) -> String {
    format!(
        "Current outgoing: {} · {} step(s) · {} µs added",
        match &transition.kind {
            TransitionKind::FadeToNext => "Fade to next",
            TransitionKind::FadeToColor { .. } => "Fade to RGBA",
            TransitionKind::Slide {
                direction: SlideDirection::Left,
            } => "Slide left",
            TransitionKind::Slide {
                direction: SlideDirection::Right,
            } => "Slide right",
            TransitionKind::Slide {
                direction: SlideDirection::Up,
            } => "Slide up",
            TransitionKind::Slide {
                direction: SlideDirection::Down,
            } => "Slide down",
        },
        transition.steps,
        transition.duration.get()
    )
}

fn show_transform_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.group(|ui| {
        ui.strong("Transform selected frames");
        show_crop_resize_controls(ui, workspace, state, now, results);
        show_orientation_controls(ui, workspace, state, now, results);
    });
}

fn show_crop_resize_controls(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Crop");
        ui.label("X");
        compact_input(ui, &mut state.crop_x_input);
        ui.label("Y");
        compact_input(ui, &mut state.crop_y_input);
        ui.label("W");
        compact_input(ui, &mut state.crop_width_input);
        ui.label("H");
        compact_input(ui, &mut state.crop_height_input);
        if ui.button("Apply crop").clicked() {
            match parse_crop(state) {
                Ok(crop) => {
                    let result = workspace.set_selection_crop(crop);
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::ApplyCrop,
                        result,
                    );
                }
                Err(message) => push_failure(results, EditorUiOperation::ApplyCrop, message),
            }
        }
        if ui.button("Clear crop").clicked() {
            let result = workspace.clear_selection_crop();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::ClearCrop,
                result,
            );
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("Resize before rotation");
        ui.label("W");
        compact_input(ui, &mut state.resize_width_input);
        ui.label("H");
        compact_input(ui, &mut state.resize_height_input);
        if ui.button("Resize").clicked() {
            match parse_output_size(state) {
                Ok(size) => {
                    let result = workspace.set_selection_output_size(size);
                    record_project_result(
                        workspace,
                        state,
                        now,
                        results,
                        EditorUiOperation::Resize,
                        result,
                    );
                }
                Err(message) => push_failure(results, EditorUiOperation::Resize, message),
            }
        }
        if ui.button("Clear resize").clicked() {
            let result = workspace.clear_selection_output_size();
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::ClearOutputSize,
                result,
            );
        }
    });
}

fn show_orientation_controls(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        for (label, control) in [
            ("Rotate left", OrientationControl::RotateLeft),
            ("Rotate right", OrientationControl::RotateRight),
            ("Flip H", OrientationControl::FlipHorizontal),
            ("Flip V", OrientationControl::FlipVertical),
        ] {
            if ui.button(label).clicked() {
                let operation = orientation_operation(control);
                let result = apply_orientation_control(workspace, control);
                record_project_result(workspace, state, now, results, operation, result);
            }
        }
    });
}

fn compact_input(ui: &mut egui::Ui, value: &mut String) {
    ui.add(egui::TextEdit::singleline(value).desired_width(54.0));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrientationControl {
    RotateLeft,
    RotateRight,
    FlipHorizontal,
    FlipVertical,
}

const fn orientation_operation(control: OrientationControl) -> EditorUiOperation {
    match control {
        OrientationControl::RotateLeft => EditorUiOperation::RotateLeft,
        OrientationControl::RotateRight => EditorUiOperation::RotateRight,
        OrientationControl::FlipHorizontal => EditorUiOperation::FlipHorizontal,
        OrientationControl::FlipVertical => EditorUiOperation::FlipVertical,
    }
}

fn apply_orientation_control(
    workspace: &mut EditorWorkspace,
    control: OrientationControl,
) -> Result<(), EditorWorkspaceError> {
    match control {
        OrientationControl::RotateLeft => workspace.rotate_selection_counterclockwise(),
        OrientationControl::RotateRight => workspace.rotate_selection_clockwise(),
        OrientationControl::FlipHorizontal => workspace.toggle_selection_horizontal_flip(),
        OrientationControl::FlipVertical => workspace.toggle_selection_vertical_flip(),
    }
}

fn parse_crop(state: &EditorUiState) -> Result<PhysicalRect, String> {
    let x = parse_input::<u32>(&state.crop_x_input, "crop X")?;
    let y = parse_input::<u32>(&state.crop_y_input, "crop Y")?;
    let width = parse_input::<u32>(&state.crop_width_input, "crop width")?;
    let height = parse_input::<u32>(&state.crop_height_input, "crop height")?;
    PhysicalRect::new(x, y, width, height).map_err(|error| format!("invalid crop: {error}"))
}

fn parse_output_size(state: &EditorUiState) -> Result<PhysicalSize, String> {
    let width = parse_input::<u32>(&state.resize_width_input, "resize width")?;
    let height = parse_input::<u32>(&state.resize_height_input, "resize height")?;
    PhysicalSize::new(width, height).map_err(|error| format!("invalid resize: {error}"))
}

fn show_effect_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Frame effects");
            egui::ComboBox::from_id_salt("frame_effect_choice")
                .selected_text(effect_choice_label(state.effect_choice))
                .show_ui(ui, |ui| {
                    for choice in [
                        EffectChoice::Blur,
                        EffectChoice::Pixelate,
                        EffectChoice::Darken,
                        EffectChoice::Lighten,
                        EffectChoice::Border,
                        EffectChoice::Shadow,
                    ] {
                        ui.selectable_value(
                            &mut state.effect_choice,
                            choice,
                            effect_choice_label(choice),
                        );
                    }
                });
            ui.label("Replace #");
            compact_input(ui, &mut state.effect_index_input);
        });
        show_effect_inputs(ui, state);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add effect").clicked() {
                match build_effect(state) {
                    Ok(effect) => {
                        let result = workspace.add_selection_effect(effect);
                        record_project_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::AddEffect,
                            result,
                        );
                    }
                    Err(message) => push_failure(results, EditorUiOperation::AddEffect, message),
                }
            }
            if ui.button("Replace effect").clicked() {
                match parse_effect_index(state)
                    .and_then(|index| build_effect(state).map(|effect| (index, effect)))
                {
                    Ok((index, effect)) => {
                        let result = workspace.replace_selection_effect(index, effect);
                        record_project_result(
                            workspace,
                            state,
                            now,
                            results,
                            EditorUiOperation::ReplaceEffect,
                            result,
                        );
                    }
                    Err(message) => {
                        push_failure(results, EditorUiOperation::ReplaceEffect, message);
                    }
                }
            }
            if ui.button("Clear selected effects").clicked() {
                let result = workspace.clear_selection_effects();
                record_project_result(
                    workspace,
                    state,
                    now,
                    results,
                    EditorUiOperation::ClearEffects,
                    result,
                );
            }
        });
    });
}

fn show_effect_inputs(ui: &mut egui::Ui, state: &mut EditorUiState) {
    match state.effect_choice {
        EffectChoice::Blur
        | EffectChoice::Pixelate
        | EffectChoice::Darken
        | EffectChoice::Lighten => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Canvas region X/Y/W/H");
                compact_input(ui, &mut state.effect_region_x_input);
                compact_input(ui, &mut state.effect_region_y_input);
                compact_input(ui, &mut state.effect_region_width_input);
                compact_input(ui, &mut state.effect_region_height_input);
                match state.effect_choice {
                    EffectChoice::Blur => {
                        ui.label("Radius");
                        compact_input(ui, &mut state.effect_blur_radius_input);
                    }
                    EffectChoice::Pixelate => {
                        ui.label("Block");
                        compact_input(ui, &mut state.effect_pixel_block_input);
                    }
                    EffectChoice::Darken | EffectChoice::Lighten => {
                        ui.label("Percent");
                        compact_input(ui, &mut state.effect_tone_percent_input);
                    }
                    EffectChoice::Border | EffectChoice::Shadow => {}
                }
            });
        }
        EffectChoice::Border => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Edges T/R/B/L");
                compact_input(ui, &mut state.effect_border_top_input);
                compact_input(ui, &mut state.effect_border_right_input);
                compact_input(ui, &mut state.effect_border_bottom_input);
                compact_input(ui, &mut state.effect_border_left_input);
                show_effect_color_inputs(ui, state);
            });
        }
        EffectChoice::Shadow => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Offset X/Y");
                compact_input(ui, &mut state.effect_shadow_offset_x_input);
                compact_input(ui, &mut state.effect_shadow_offset_y_input);
                ui.label("Blur");
                compact_input(ui, &mut state.effect_shadow_blur_input);
                show_effect_color_inputs(ui, state);
            });
        }
    }
}

fn show_effect_color_inputs(ui: &mut egui::Ui, state: &mut EditorUiState) {
    ui.label("RGBA");
    compact_input(ui, &mut state.effect_color_red_input);
    compact_input(ui, &mut state.effect_color_green_input);
    compact_input(ui, &mut state.effect_color_blue_input);
    compact_input(ui, &mut state.effect_color_alpha_input);
}

const fn effect_choice_label(choice: EffectChoice) -> &'static str {
    match choice {
        EffectChoice::Blur => "Blur",
        EffectChoice::Pixelate => "Pixelate",
        EffectChoice::Darken => "Darken",
        EffectChoice::Lighten => "Lighten",
        EffectChoice::Border => "Border",
        EffectChoice::Shadow => "Shadow",
    }
}

fn build_effect(state: &EditorUiState) -> Result<Effect, String> {
    match state.effect_choice {
        EffectChoice::Blur => {
            let radius = parse_input::<u16>(&state.effect_blur_radius_input, "blur radius")?;
            if !(1..=MAX_FRAME_EFFECT_BLUR_RADIUS).contains(&radius) {
                return Err(format!(
                    "blur radius must be between 1 and {MAX_FRAME_EFFECT_BLUR_RADIUS}"
                ));
            }
            Ok(Effect::Blur {
                region: parse_effect_region(state)?,
                radius,
            })
        }
        EffectChoice::Pixelate => {
            let block_size =
                parse_input::<u16>(&state.effect_pixel_block_input, "pixel block size")?;
            if block_size == 0 {
                return Err("pixel block size must be positive".to_owned());
            }
            Ok(Effect::Pixelate {
                region: parse_effect_region(state)?,
                block_size,
            })
        }
        EffectChoice::Darken | EffectChoice::Lighten => {
            let amount_percent =
                parse_input::<u8>(&state.effect_tone_percent_input, "tone percentage")?;
            if amount_percent > 100 {
                return Err("tone percentage must be between 0 and 100".to_owned());
            }
            let region = parse_effect_region(state)?;
            if state.effect_choice == EffectChoice::Darken {
                Ok(Effect::Darken {
                    region,
                    amount_percent,
                })
            } else {
                Ok(Effect::Lighten {
                    region,
                    amount_percent,
                })
            }
        }
        EffectChoice::Border => {
            let widths = EdgeWidths {
                top: parse_input::<u16>(&state.effect_border_top_input, "border top")?,
                right: parse_input::<u16>(&state.effect_border_right_input, "border right")?,
                bottom: parse_input::<u16>(&state.effect_border_bottom_input, "border bottom")?,
                left: parse_input::<u16>(&state.effect_border_left_input, "border left")?,
            };
            if widths == EdgeWidths::default() {
                return Err("at least one border edge must be positive".to_owned());
            }
            Ok(Effect::Border {
                widths,
                color: parse_effect_color(state)?,
            })
        }
        EffectChoice::Shadow => {
            let blur_radius =
                parse_input::<u16>(&state.effect_shadow_blur_input, "shadow blur radius")?;
            if blur_radius > MAX_FRAME_EFFECT_BLUR_RADIUS {
                return Err(format!(
                    "shadow blur radius must be at most {MAX_FRAME_EFFECT_BLUR_RADIUS}"
                ));
            }
            Ok(Effect::Shadow {
                offset_x: parse_input::<i32>(&state.effect_shadow_offset_x_input, "shadow X")?,
                offset_y: parse_input::<i32>(&state.effect_shadow_offset_y_input, "shadow Y")?,
                blur_radius,
                color: parse_effect_color(state)?,
            })
        }
    }
}

fn parse_effect_region(state: &EditorUiState) -> Result<PhysicalRect, String> {
    let x = parse_input::<u32>(&state.effect_region_x_input, "effect region X")?;
    let y = parse_input::<u32>(&state.effect_region_y_input, "effect region Y")?;
    let width = parse_input::<u32>(&state.effect_region_width_input, "effect region width")?;
    let height = parse_input::<u32>(&state.effect_region_height_input, "effect region height")?;
    PhysicalRect::new(x, y, width, height)
        .map_err(|error| format!("invalid effect region: {error}"))
}

fn parse_effect_color(state: &EditorUiState) -> Result<Rgba, String> {
    let color = Rgba {
        red: parse_input::<u8>(&state.effect_color_red_input, "effect red")?,
        green: parse_input::<u8>(&state.effect_color_green_input, "effect green")?,
        blue: parse_input::<u8>(&state.effect_color_blue_input, "effect blue")?,
        alpha: parse_input::<u8>(&state.effect_color_alpha_input, "effect alpha")?,
    };
    if color.alpha == 0 {
        return Err("effect color alpha must be positive".to_owned());
    }
    Ok(color)
}

fn parse_effect_index(state: &EditorUiState) -> Result<usize, String> {
    parse_input::<usize>(&state.effect_index_input, "effect number")?
        .checked_sub(1)
        .ok_or_else(|| "effect number is 1-based and must be positive".to_owned())
}

fn show_shape_overlay_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Shape overlays");
            ui.weak("Applies to selected frames only; gaps in the selection stay unchanged.");
        });
        show_shape_overlay_inputs(ui, &mut state.shape_overlay);
        if ui.button("Add shape overlay").clicked() {
            let result =
                build_shape_overlay(&state.shape_overlay, workspace.manifest().canvas.size)
                    .and_then(|content| {
                        workspace
                            .add_overlay_for_selection(
                                state.shape_overlay.name.trim().to_owned(),
                                content,
                                state.shape_overlay.z_index,
                                state.shape_overlay.track_opacity,
                                state.shape_overlay.blend_mode,
                            )
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
            record_project_result(
                workspace,
                state,
                now,
                results,
                EditorUiOperation::AddShapeOverlay,
                result,
            );
        }
    });
}

fn show_shape_overlay_inputs(ui: &mut egui::Ui, state: &mut ShapeOverlayUiState) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(120.0));
        ui.label("Kind");
        egui::ComboBox::from_id_salt("shape_overlay_kind")
            .selected_text(shape_overlay_choice_label(state.kind))
            .show_ui(ui, |ui| {
                for choice in [
                    ShapeOverlayChoice::Line,
                    ShapeOverlayChoice::Arrow,
                    ShapeOverlayChoice::Rectangle,
                    ShapeOverlayChoice::Ellipse,
                ] {
                    ui.selectable_value(
                        &mut state.kind,
                        choice,
                        shape_overlay_choice_label(choice),
                    );
                }
            });
        ui.label("Z");
        ui.add(egui::DragValue::new(&mut state.z_index));
        ui.label("Track opacity");
        ui.add(egui::DragValue::new(&mut state.track_opacity).range(1..=u8::MAX));
        ui.label("Blend");
        egui::ComboBox::from_id_salt("shape_overlay_blend")
            .selected_text(blend_mode_label(state.blend_mode))
            .show_ui(ui, |ui| {
                for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
                    ui.selectable_value(&mut state.blend_mode, blend, blend_mode_label(blend));
                }
            });
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Bounds X/Y/W/H");
        ui.add(egui::DragValue::new(&mut state.x));
        ui.add(egui::DragValue::new(&mut state.y));
        ui.add(egui::DragValue::new(&mut state.width).range(1..=u32::MAX));
        ui.add(egui::DragValue::new(&mut state.height).range(1..=u32::MAX));
        ui.label("Stroke width");
        ui.add(egui::DragValue::new(&mut state.stroke_width));
    });
    ui.horizontal_wrapped(|ui| {
        show_rgba_inputs(ui, "Stroke RGBA", &mut state.stroke);
        ui.checkbox(&mut state.fill_enabled, "Fill");
        ui.add_enabled_ui(state.fill_enabled, |ui| {
            show_rgba_inputs(ui, "Fill RGBA", &mut state.fill);
        });
    });
}

fn show_rgba_inputs(ui: &mut egui::Ui, label: &str, color: &mut Rgba) {
    ui.label(label);
    ui.add(egui::DragValue::new(&mut color.red));
    ui.add(egui::DragValue::new(&mut color.green));
    ui.add(egui::DragValue::new(&mut color.blue));
    ui.add(egui::DragValue::new(&mut color.alpha));
}

fn show_drawing_overlay_controls(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    state.drawing_overlay.reconcile(workspace);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Free drawing");
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut state.drawing_overlay.name).desired_width(110.0));
        ui.label("Width");
        ui.add(egui::DragValue::new(&mut state.drawing_overlay.width).range(1..=u16::MAX));
        show_rgba_inputs(ui, "RGBA", &mut state.drawing_overlay.color);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Z");
        ui.add(egui::DragValue::new(&mut state.drawing_overlay.z_index));
        ui.label("Track opacity");
        ui.add(egui::DragValue::new(&mut state.drawing_overlay.track_opacity).range(1..=u8::MAX));
        ui.label("Blend");
        egui::ComboBox::from_id_salt("drawing_overlay_blend")
            .selected_text(blend_mode_label(state.drawing_overlay.blend_mode))
            .show_ui(ui, |ui| {
                for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
                    ui.selectable_value(
                        &mut state.drawing_overlay.blend_mode,
                        blend,
                        blend_mode_label(blend),
                    );
                }
            });
        match state.drawing_overlay.phase {
            DrawingDraftPhase::Idle => {
                if ui
                    .add_enabled(
                        !workspace.selection().is_empty(),
                        egui::Button::new("Draw one stroke on preview"),
                    )
                    .clicked()
                    && let Err(error) = state.drawing_overlay.begin_for_selection(workspace)
                {
                    push_failure(results, EditorUiOperation::AddDrawingOverlay, error);
                }
            }
            DrawingDraftPhase::Capturing => {
                ui.strong("Drag once across the current preview.");
                if ui.button("Cancel stroke").clicked() {
                    state.drawing_overlay.cancel();
                }
            }
            DrawingDraftPhase::Ready => {
                ui.label(format!("{} point(s)", state.drawing_overlay.points.len()));
                if ui.button("Commit drawing overlay").clicked() {
                    commit_drawing_overlay(workspace, state, now, results);
                }
                if ui.button("Cancel stroke").clicked() {
                    state.drawing_overlay.cancel();
                }
            }
        }
    });
    if state.drawing_overlay.limit_reached {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("Stroke stopped at the {MAX_DRAWING_DRAFT_POINTS}-point safety limit."),
        );
    }
}

fn commit_drawing_overlay(
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let result = build_drawing_overlay(&state.drawing_overlay).and_then(|content| {
        workspace
            .add_overlay_for_selection(
                state.drawing_overlay.name.trim().to_owned(),
                content,
                state.drawing_overlay.z_index,
                state.drawing_overlay.track_opacity,
                state.drawing_overlay.blend_mode,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    if result.is_ok() {
        state.drawing_overlay.cancel();
    }
    record_project_result(
        workspace,
        state,
        now,
        results,
        EditorUiOperation::AddDrawingOverlay,
        result,
    );
}

fn build_drawing_overlay(draft: &DrawingOverlayDraft) -> Result<OverlayContent, String> {
    if draft.name.trim().is_empty() {
        return Err("Drawing overlay name is required.".to_owned());
    }
    if draft.width == 0 || draft.color.alpha == 0 || draft.track_opacity == 0 {
        return Err("Drawing width, color alpha, and track opacity must be visible.".to_owned());
    }
    if draft.points.is_empty() {
        return Err("Draw at least one point on the preview before committing.".to_owned());
    }
    if draft.points.len() > MAX_DRAWING_DRAFT_POINTS {
        return Err(format!(
            "Drawing contains more than {MAX_DRAWING_DRAFT_POINTS} points."
        ));
    }
    if draft
        .points
        .iter()
        .any(|point| point.pressure_milli > 1_000)
    {
        return Err("Drawing pressure must stay in 0..=1000.".to_owned());
    }
    Ok(OverlayContent::Drawing {
        points: draft.points.clone(),
        width: draft.width,
        color: draft.color,
    })
}

fn show_overlay_track_list(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let tracks = workspace
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .take(MAX_VISIBLE_OVERLAY_TRACKS)
        .map(|track| {
            (
                track.id,
                track.name.clone(),
                track
                    .items
                    .first()
                    .map(|item| overlay_content_label(&item.content)),
                track.items.len(),
            )
        })
        .collect::<Vec<_>>();
    if tracks.is_empty() {
        return;
    }
    ui.separator();
    ui.label(format!(
        "Overlay tracks: {}{}",
        workspace.manifest().timeline.overlay_tracks.len(),
        if workspace.manifest().timeline.overlay_tracks.len() > MAX_VISIBLE_OVERLAY_TRACKS {
            " (showing first 64)"
        } else {
            ""
        }
    ));
    let mut remove = None;
    for (track_id, name, kind, item_count) in tracks {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{name} · {} · {item_count} item(s)",
                kind.unwrap_or("Empty")
            ));
            if ui.small_button("Remove track").clicked() {
                remove = Some(track_id);
            }
        });
    }
    if let Some(track_id) = remove {
        let result = workspace.remove_overlay_track(track_id);
        record_project_result(
            workspace,
            state,
            now,
            results,
            EditorUiOperation::RemoveOverlayTrack,
            result,
        );
    }
}

fn build_shape_overlay(
    state: &ShapeOverlayUiState,
    canvas: PhysicalSize,
) -> Result<OverlayContent, String> {
    if state.name.trim().is_empty() {
        return Err("Shape overlay name is required.".to_owned());
    }
    if state.track_opacity == 0 {
        return Err("Shape track opacity must be greater than zero.".to_owned());
    }
    let bounds = PhysicalRect::new(state.x, state.y, state.width, state.height)
        .map_err(|error| format!("Invalid shape bounds: {error}"))?;
    if !bounds.fits_within(canvas) {
        return Err("Shape bounds must stay inside the rendered canvas.".to_owned());
    }
    let kind = shape_kind(state.kind);
    let fill = state.fill_enabled.then_some(state.fill);
    if matches!(kind, ShapeKind::Line | ShapeKind::Arrow)
        && (state.stroke_width == 0 || state.stroke.alpha == 0)
    {
        return Err("Line and arrow overlays require a visible positive-width stroke.".to_owned());
    }
    if matches!(kind, ShapeKind::Rectangle | ShapeKind::Ellipse)
        && (state.stroke_width == 0 || state.stroke.alpha == 0)
        && fill.is_none_or(|color| color.alpha == 0)
    {
        return Err("Rectangle and ellipse overlays require a visible stroke or fill.".to_owned());
    }
    Ok(OverlayContent::Shape {
        kind,
        bounds,
        stroke_width: state.stroke_width,
        stroke: state.stroke,
        fill,
    })
}

const fn shape_kind(choice: ShapeOverlayChoice) -> ShapeKind {
    match choice {
        ShapeOverlayChoice::Line => ShapeKind::Line,
        ShapeOverlayChoice::Arrow => ShapeKind::Arrow,
        ShapeOverlayChoice::Rectangle => ShapeKind::Rectangle,
        ShapeOverlayChoice::Ellipse => ShapeKind::Ellipse,
    }
}

const fn shape_overlay_choice_label(choice: ShapeOverlayChoice) -> &'static str {
    match choice {
        ShapeOverlayChoice::Line => "Line",
        ShapeOverlayChoice::Arrow => "Arrow",
        ShapeOverlayChoice::Rectangle => "Rectangle",
        ShapeOverlayChoice::Ellipse => "Ellipse",
    }
}

const fn blend_mode_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
    }
}

const fn overlay_content_label(content: &OverlayContent) -> &'static str {
    match content {
        OverlayContent::Raster { .. } => "Raster",
        OverlayContent::Text { .. } => "Text",
        OverlayContent::Shape { .. } => "Shape",
        OverlayContent::Drawing { .. } => "Drawing",
        OverlayContent::KeyStroke { .. } => "Key stroke",
        OverlayContent::Cursor { .. } => "Cursor",
        OverlayContent::MouseClick { .. } => "Mouse click",
        OverlayContent::Progress { .. } => "Progress",
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "virtual range calculation and its only widget loop are intentionally co-located"
)]
fn show_virtual_filmstrip(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let frame_count = workspace.manifest().timeline.frames.len();
    let layout = match filmstrip_layout(frame_count) {
        Ok(layout) => layout,
        Err(error) => {
            push_failure(results, EditorUiOperation::FilmstripLayout, error);
            return;
        }
    };
    let available_width = f64::from(ui.available_width().max(1.0));
    if state.reveal_current_frame {
        if let Some(index) = current_frame_index(workspace) {
            match layout.scroll_offset_to_reveal(
                index,
                state.filmstrip_scroll_offset,
                available_width,
            ) {
                Ok(offset) => state.filmstrip_scroll_offset = offset,
                Err(error) => push_failure(results, EditorUiOperation::FilmstripLayout, error),
            }
        }
        state.reveal_current_frame = false;
    }

    let requested_offset = match to_ui_points(state.filmstrip_scroll_offset) {
        Ok(value) => value,
        Err(message) => {
            push_failure(results, EditorUiOperation::FilmstripLayout, message);
            0.0
        }
    };
    let total_width = match to_ui_points(layout.total_content_width()) {
        Ok(value) => value,
        Err(message) => {
            push_failure(results, EditorUiOperation::FilmstripLayout, message);
            return;
        }
    };
    let item_width = match to_ui_points(layout.item_width()) {
        Ok(value) => value,
        Err(message) => {
            push_failure(results, EditorUiOperation::FilmstripLayout, message);
            return;
        }
    };

    let output = egui::ScrollArea::horizontal()
        .id_salt("editor_virtual_filmstrip")
        .auto_shrink([false, true])
        .max_height(FILMSTRIP_ITEM_HEIGHT + 18.0)
        .horizontal_scroll_offset(requested_offset)
        .show_viewport(ui, |ui, viewport| {
            ui.set_min_size(egui::vec2(total_width, FILMSTRIP_ITEM_HEIGHT));
            let viewport_offset = f64::from(viewport.min.x.max(0.0));
            let viewport_width = f64::from(viewport.width().max(f32::EPSILON));
            let visible =
                match layout.visible_range(viewport_offset, viewport_width, FILMSTRIP_OVERSCAN) {
                    Ok(range) => range,
                    Err(error) => {
                        push_failure(results, EditorUiOperation::FilmstripLayout, error);
                        return;
                    }
                };
            state.thumbnail_cache.set_visible(
                workspace.active_project(),
                visible.clone(),
                ui.ctx(),
                [100, 48],
            );
            let content_origin = ui.min_rect().min;
            for index in visible {
                let x = match layout.x_for_index(index).and_then(|value| {
                    to_ui_points(value).map_err(|_| VirtualFilmstripError::CoordinateOverflow)
                }) {
                    Ok(value) => value,
                    Err(error) => {
                        push_failure(results, EditorUiOperation::FilmstripLayout, error);
                        break;
                    }
                };
                let (frame_id, duration_us) = {
                    let frame = &workspace.manifest().timeline.frames[index];
                    (frame.id, frame.duration.get())
                };
                let is_selected = workspace.selection().contains(frame_id);
                let is_current = workspace.selection().current() == Some(frame_id);
                let label = format!("Frame {} · {} µs", index + 1, duration_us);
                let mut button = egui::Button::new("")
                    .min_size(egui::vec2(item_width, FILMSTRIP_ITEM_HEIGHT))
                    .selected(is_selected);
                if is_current {
                    button = button.stroke(egui::Stroke::new(
                        2.0_f32,
                        ui.visuals().selection.stroke.color,
                    ));
                }
                let rect = egui::Rect::from_min_size(
                    egui::pos2(content_origin.x + x, content_origin.y),
                    egui::vec2(item_width, FILMSTRIP_ITEM_HEIGHT),
                );
                // Cards share one horizontal row. A scope that advances the
                // parent cursor would add vertical spacing for every card.
                let mut response = ui
                    .new_child(
                        egui::UiBuilder::new()
                            .id_salt(("editor-frame-card", index))
                            .max_rect(rect)
                            .layout(egui::Layout::centered_and_justified(
                                egui::Direction::TopDown,
                            )),
                    )
                    .add(button);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        ui.is_enabled(),
                        is_selected,
                        &label,
                    )
                });
                let image_bounds = egui::Rect::from_min_max(
                    rect.min + egui::vec2(6.0, 4.0),
                    egui::pos2(rect.right() - 6.0, rect.top() + 47.0),
                );
                match state.thumbnail_cache.get(frame_id) {
                    Some(Ok(preview)) => {
                        let natural = preview.texture.size_vec2();
                        let scale = (image_bounds.width() / natural.x)
                            .min(image_bounds.height() / natural.y);
                        let image_rect =
                            egui::Rect::from_center_size(image_bounds.center(), natural * scale);
                        ui.painter().image(
                            preview.texture.id(),
                            image_rect,
                            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                        response = response.on_hover_text(&label);
                    }
                    Some(Err(error)) => {
                        paint_thumbnail_placeholder(ui, image_bounds, "Unavailable");
                        response = response.on_hover_text(format!("{label}\n{error}"));
                    }
                    None => paint_thumbnail_placeholder(ui, image_bounds, "Loading…"),
                }
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.top() + 53.0),
                    egui::Align2::CENTER_CENTER,
                    format!("Frame {}", index + 1),
                    egui::FontId::proportional(11.0),
                    ui.visuals().text_color(),
                );
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.top() + 68.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{}.{:03} ms", duration_us / 1_000, duration_us % 1_000),
                    egui::FontId::proportional(10.0),
                    ui.visuals().weak_text_color(),
                );
                if response.clicked() {
                    let modifiers = ui.input(|input| input.modifiers);
                    let operation = frame_click_operation(modifiers.ctrl, modifiers.shift);
                    let result = match operation {
                        EditorUiOperation::ToggleFrame => workspace.toggle_selection(frame_id),
                        EditorUiOperation::ExtendFrameRange => {
                            workspace.extend_selection_range(frame_id)
                        }
                        _ => workspace.select_only(frame_id),
                    };
                    record_selection_result(workspace, state, now, results, operation, result);
                }
            }
        });
    state.filmstrip_scroll_offset = f64::from(output.state.offset.x.max(0.0));
}

fn paint_thumbnail_placeholder(ui: &egui::Ui, bounds: egui::Rect, text: &str) {
    ui.painter()
        .rect_filled(bounds, 2.0, ui.visuals().faint_bg_color);
    ui.painter().text(
        bounds.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.0),
        ui.visuals().weak_text_color(),
    );
}

fn filmstrip_layout(frame_count: usize) -> Result<VirtualFilmstripLayout, VirtualFilmstripError> {
    VirtualFilmstripLayout::new(frame_count, FILMSTRIP_ITEM_WIDTH, FILMSTRIP_ITEM_GAP)
}

fn visible_widget_range(
    frame_count: usize,
    viewport_offset: f64,
    viewport_width: f64,
) -> Result<Range<usize>, VirtualFilmstripError> {
    filmstrip_layout(frame_count)?.visible_range(
        viewport_offset,
        viewport_width,
        FILMSTRIP_OVERSCAN,
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "this is the explicit f64-to-egui-f32 boundary after finite range validation"
)]
fn to_ui_points(value: f64) -> Result<f32, &'static str> {
    if !value.is_finite() || value < 0.0 || value > f64::from(f32::MAX) {
        return Err("filmstrip coordinate cannot be represented by egui");
    }
    Ok(value as f32)
}

fn frame_click_operation(ctrl: bool, shift: bool) -> EditorUiOperation {
    if shift {
        EditorUiOperation::ExtendFrameRange
    } else if ctrl {
        EditorUiOperation::ToggleFrame
    } else {
        EditorUiOperation::SelectFrame
    }
}

fn apply_frame_expression(
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let frame_ids =
        match parse_frame_expression(&workspace.manifest().timeline, &state.frame_expression) {
            Ok(frame_ids) => frame_ids,
            Err(error) => {
                push_failure(results, EditorUiOperation::SelectExpression, error);
                return;
            }
        };
    let result = replace_selection(workspace, &frame_ids);
    record_selection_result(
        workspace,
        state,
        now,
        results,
        EditorUiOperation::SelectExpression,
        result,
    );
}

fn replace_selection(
    workspace: &mut EditorWorkspace,
    frame_ids: &[FrameId],
) -> Result<(), EditorWorkspaceError> {
    let Some((first, remaining)) = frame_ids.split_first() else {
        workspace.clear_selection();
        return Ok(());
    };
    workspace.select_only(*first)?;
    for frame_id in remaining {
        workspace.toggle_selection(*frame_id)?;
    }
    Ok(())
}

fn parse_input<T>(input: &str, label: &str) -> Result<T, String>
where
    T: FromStr,
    T::Err: Display,
{
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required"));
    }
    trimmed
        .parse()
        .map_err(|error| format!("invalid {label}: {error}"))
}

fn parse_time_ms(input: &str) -> Result<TimeUs, String> {
    let milliseconds = parse_input::<u64>(input, "time in milliseconds")?;
    let microseconds = milliseconds
        .checked_mul(1_000)
        .ok_or_else(|| "time in milliseconds is too large".to_owned())?;
    Ok(TimeUs::new(microseconds))
}

fn parse_time_range(state: &EditorUiState) -> Result<(TimeUs, TimeUs), String> {
    Ok((
        parse_time_ms(&state.time_range_start_ms_input)?,
        parse_time_ms(&state.time_range_end_ms_input)?,
    ))
}

fn parse_duration_us(input: &str) -> Result<DurationUs, String> {
    let microseconds = parse_input::<u64>(input, "frame delay in microseconds")?;
    DurationUs::new(microseconds).ok_or_else(|| "frame delay must be positive".to_owned())
}

fn record_selection_result<T, E>(
    workspace: &EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
    result: Result<T, E>,
) where
    E: Display,
{
    match result {
        Ok(_) => selection_succeeded(workspace, state, now, results, operation),
        Err(error) => push_failure(results, operation, error),
    }
}

fn selection_succeeded(
    workspace: &EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
) {
    results.push(Ok(EditorUiAction::Selection(operation)));
    state.reveal_current_frame = true;
    synchronize_playback_after_selection(workspace, state, now, results);
}

fn record_project_result<E>(
    workspace: &EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
    result: Result<(), E>,
) where
    E: Display,
{
    match result {
        Ok(()) => {
            results.push(Ok(EditorUiAction::Project(operation)));
            state.reveal_current_frame = true;
            synchronize_playback_after_selection(workspace, state, now, results);
        }
        Err(error) => push_failure(results, operation, error),
    }
}

fn record_history_result(
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
    undo: bool,
) {
    let result = if undo {
        workspace.undo()
    } else {
        workspace.redo()
    };
    match result {
        Ok(true) => {
            results.push(Ok(EditorUiAction::Project(operation)));
            state.reveal_current_frame = true;
            synchronize_playback_after_selection(workspace, state, now, results);
        }
        Ok(false) => {}
        Err(error) => push_failure(results, operation, error),
    }
}

fn push_failure(
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
    error: impl Display,
) {
    results.push(Err(EditorUiFailure {
        operation,
        message: error.to_string(),
    }));
}

fn push_notice(
    results: &mut Vec<EditorUiResult>,
    operation: EditorUiOperation,
    message: impl Into<String>,
) {
    results.push(Ok(EditorUiAction::Notice {
        operation,
        message: message.into(),
    }));
}

fn current_frame_index(workspace: &EditorWorkspace) -> Option<usize> {
    let current = workspace.selection().current()?;
    workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .position(|frame| frame.id == current)
}

fn playback_clock_for_current(workspace: &EditorWorkspace, now: Instant) -> Option<PlaybackClock> {
    let index = current_frame_index(workspace)?;
    let frame = workspace.manifest().timeline.frames.get(index)?;
    Some(PlaybackClock::new(frame.id, now, frame.duration))
}

fn toggle_playback(
    context: &egui::Context,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    if state.playback.take().is_some() {
        results.push(Ok(EditorUiAction::Playback { playing: false }));
        return;
    }
    if workspace.selection().current().is_none() {
        match workspace.select_first() {
            Ok(_) => results.push(Ok(EditorUiAction::Selection(
                EditorUiOperation::SelectFirst,
            ))),
            Err(error) => {
                push_failure(results, EditorUiOperation::Playback, error);
                return;
            }
        }
    }
    let Some(clock) = playback_clock_for_current(workspace, now) else {
        push_failure(
            results,
            EditorUiOperation::Playback,
            "the current frame is unavailable",
        );
        return;
    };
    state.playback = Some(clock);
    state.reveal_current_frame = true;
    results.push(Ok(EditorUiAction::Playback { playing: true }));
    schedule_playback_repaint(context, state, now);
}

fn advance_playback(
    context: &egui::Context,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    let Some(clock) = state.playback else {
        return;
    };
    let Some(index) = current_frame_index(workspace) else {
        state.playback = None;
        results.push(Ok(EditorUiAction::Playback { playing: false }));
        return;
    };
    let current_id = workspace.manifest().timeline.frames[index].id;
    if current_id != clock.id {
        state.playback = playback_clock_for_current(workspace, now);
        schedule_playback_repaint(context, state, now);
        return;
    }
    if !clock.is_due(now) {
        schedule_playback_repaint(context, state, now);
        return;
    }
    let Some((frame_id, next_clock)) = clock.advance(
        &workspace.manifest().timeline.frames,
        index,
        now,
        state.loop_preview,
    ) else {
        state.playback = None;
        push_failure(
            results,
            EditorUiOperation::PlaybackStep,
            "Playback timing is out of range.",
        );
        results.push(Ok(EditorUiAction::Playback { playing: false }));
        return;
    };
    if frame_id != current_id {
        if let Err(error) = workspace.select_only(frame_id) {
            state.playback = None;
            push_failure(results, EditorUiOperation::PlaybackStep, error);
            results.push(Ok(EditorUiAction::Playback { playing: false }));
            return;
        }
        results.push(Ok(EditorUiAction::Selection(
            EditorUiOperation::PlaybackStep,
        )));
        state.reveal_current_frame = true;
    }
    state.playback = next_clock;
    if next_clock.is_none() {
        results.push(Ok(EditorUiAction::Playback { playing: false }));
    }
    schedule_playback_repaint(context, state, now);
}

fn synchronize_playback_after_selection(
    workspace: &EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    if state.playback.is_none() {
        return;
    }
    state.playback = playback_clock_for_current(workspace, now);
    if state.playback.is_none() {
        results.push(Ok(EditorUiAction::Playback { playing: false }));
    }
}

fn schedule_playback_repaint(context: &egui::Context, state: &EditorUiState, now: Instant) {
    if let Some(clock) = state.playback {
        let remaining = clock.remaining(now);
        if remaining.is_zero() {
            context.request_repaint();
        } else {
            context.request_repaint_after(remaining);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gif_from_screen_domain::{
        AssetId, BlendMode, CaptureMetadata, ClipTransform, DurationUs, Effect, FrameClip, FrameId,
        MAX_TRANSITION_STEPS, OverlayContent, PhysicalPoint, PhysicalPx, PhysicalRect,
        PhysicalSize, Rgba, ShapeKind, SlideDirection, StrokePoint, TimeUs, TransitionKind,
    };
    use gif_from_screen_editor::{
        DuplicateDelayMode, DuplicateFrameRetention, ReduceDelayMode, YoyoScope,
    };

    use super::{
        DrawingDraftPhase, DrawingOverlayDraft, EditorUiAction, EditorUiOperation, EditorUiState,
        EffectChoice, FILMSTRIP_ITEM_WIDTH, MAX_DRAWING_DRAFT_POINTS, OrientationControl,
        PlaybackClock, ShapeOverlayChoice, ShapeOverlayUiState, TransitionChoice,
        build_drawing_overlay, build_effect, build_shape_overlay, build_transition_settings,
        duplicate_delay_label, duplicate_retention_label, effect_choice_label, format_duration_us,
        format_optional_duration, frame_click_operation, orientation_operation, parse_crop,
        parse_duration_us, parse_effect_index, parse_keep_every, parse_output_size,
        parse_similarity_threshold, parse_time_ms, parse_time_range, push_notice,
        reduce_delay_label, repair_journal_notice, to_ui_points, transition_choice_label,
        visible_widget_range, yoyo_scope_label,
    };

    #[test]
    fn default_state_has_editable_inputs_and_no_playback_clock() {
        let state = EditorUiState::default();
        assert_eq!(state.frame_number_input, "1");
        assert_eq!(state.time_ms_input, "0");
        assert_eq!(state.time_range_start_ms_input, "0");
        assert_eq!(state.time_range_end_ms_input, "1000");
        assert_eq!(state.duration_us_input, "100000");
        assert_eq!(state.percentage_input, "100");
        assert_eq!(state.reduce_keep_every_input, "2");
        assert_eq!(state.reduce_delay_mode, ReduceDelayMode::DontAdjust);
        assert_eq!(state.yoyo_scope, YoyoScope::Selection);
        assert!(!state.yoyo_repeat_endpoints);
        assert_eq!(state.duplicate_threshold_input, "100");
        assert_eq!(state.duplicate_retention, DuplicateFrameRetention::First);
        assert_eq!(state.duplicate_delay_mode, DuplicateDelayMode::Sum);
        assert_eq!(state.effect_choice, EffectChoice::Blur);
        assert_eq!(state.effect_index_input, "1");
        assert_eq!(state.effect_region_x_input, "0");
        assert_eq!(state.effect_region_y_input, "0");
        assert_eq!(state.effect_region_width_input, "1");
        assert_eq!(state.effect_region_height_input, "1");
        assert_eq!(state.effect_color_alpha_input, "255");
        assert_eq!(state.frame_expression, "1");
        assert_eq!(state.transition_choice, TransitionChoice::FadeToNext);
        assert_eq!(state.transition_duration_us_input, "100000");
        assert_eq!(state.transition_steps_input, "5");
        assert_eq!(state.transition_color_red_input, "0");
        assert_eq!(state.transition_color_green_input, "0");
        assert_eq!(state.transition_color_blue_input, "0");
        assert_eq!(state.transition_color_alpha_input, "255");
        assert_eq!(state.crop_x_input, "0");
        assert_eq!(state.crop_y_input, "0");
        assert_eq!(state.crop_width_input, "1");
        assert_eq!(state.crop_height_input, "1");
        assert_eq!(state.resize_width_input, "1");
        assert_eq!(state.resize_height_input, "1");
        assert!(state.playback.is_none());
    }

    #[test]
    fn shape_overlay_inputs_build_every_kind_and_reject_invisible_or_outside_geometry() {
        let canvas = PhysicalSize::new(200, 160).unwrap();
        for (choice, expected) in [
            (ShapeOverlayChoice::Line, ShapeKind::Line),
            (ShapeOverlayChoice::Arrow, ShapeKind::Arrow),
            (ShapeOverlayChoice::Rectangle, ShapeKind::Rectangle),
            (ShapeOverlayChoice::Ellipse, ShapeKind::Ellipse),
        ] {
            let state = ShapeOverlayUiState {
                kind: choice,
                blend_mode: BlendMode::Multiply,
                ..ShapeOverlayUiState::default()
            };
            assert!(matches!(
                build_shape_overlay(&state, canvas).unwrap(),
                OverlayContent::Shape { kind, .. } if kind == expected
            ));
        }

        for state in [
            ShapeOverlayUiState {
                name: "  ".to_owned(),
                ..ShapeOverlayUiState::default()
            },
            ShapeOverlayUiState {
                x: 190,
                width: 20,
                ..ShapeOverlayUiState::default()
            },
            ShapeOverlayUiState {
                track_opacity: 0,
                ..ShapeOverlayUiState::default()
            },
            ShapeOverlayUiState {
                kind: ShapeOverlayChoice::Line,
                stroke_width: 0,
                ..ShapeOverlayUiState::default()
            },
            ShapeOverlayUiState {
                stroke_width: 0,
                stroke: Rgba::TRANSPARENT,
                fill_enabled: false,
                ..ShapeOverlayUiState::default()
            },
        ] {
            assert!(build_shape_overlay(&state, canvas).is_err());
        }

        let filled = ShapeOverlayUiState {
            stroke_width: 0,
            stroke: Rgba::TRANSPARENT,
            fill_enabled: true,
            fill: Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 255,
            },
            ..ShapeOverlayUiState::default()
        };
        assert!(build_shape_overlay(&filled, canvas).is_ok());
    }

    #[test]
    fn drawing_draft_is_bounded_deduplicated_and_builds_pressure_content() {
        let mut draft = DrawingOverlayDraft::default();
        draft.begin();
        assert_eq!(draft.phase, DrawingDraftPhase::Capturing);
        let first = StrokePoint {
            point: PhysicalPoint {
                x: PhysicalPx::new(1),
                y: PhysicalPx::new(2),
            },
            pressure_milli: 1_000,
        };
        draft.push_point(first.clone());
        draft.push_point(first);
        assert_eq!(draft.points.len(), 1);
        draft.finish_stroke();
        assert_eq!(draft.phase, DrawingDraftPhase::Ready);
        assert!(matches!(
            build_drawing_overlay(&draft).unwrap(),
            OverlayContent::Drawing { points, width: 4, .. } if points.len() == 1
        ));

        draft.begin();
        for index in 0..=MAX_DRAWING_DRAFT_POINTS {
            draft.push_point(StrokePoint {
                point: PhysicalPoint {
                    x: PhysicalPx::new(u32::try_from(index).unwrap()),
                    y: PhysicalPx::ZERO,
                },
                pressure_milli: 1_000,
            });
        }
        assert_eq!(draft.points.len(), MAX_DRAWING_DRAFT_POINTS);
        assert!(draft.limit_reached);
        assert_eq!(draft.phase, DrawingDraftPhase::Ready);
        draft.cancel();
        assert!(draft.points.is_empty());
        assert_eq!(draft.phase, DrawingDraftPhase::Idle);

        for invalid in [
            DrawingOverlayDraft {
                name: String::new(),
                points: vec![StrokePoint {
                    point: PhysicalPoint::default(),
                    pressure_milli: 1_000,
                }],
                ..DrawingOverlayDraft::default()
            },
            DrawingOverlayDraft {
                width: 0,
                points: vec![StrokePoint {
                    point: PhysicalPoint::default(),
                    pressure_milli: 1_000,
                }],
                ..DrawingOverlayDraft::default()
            },
            DrawingOverlayDraft {
                points: vec![StrokePoint {
                    point: PhysicalPoint::default(),
                    pressure_milli: 1_001,
                }],
                ..DrawingOverlayDraft::default()
            },
        ] {
            assert!(build_drawing_overlay(&invalid).is_err());
        }
    }

    #[test]
    fn click_modifiers_map_to_conventional_selection_actions() {
        assert_eq!(
            frame_click_operation(false, false),
            EditorUiOperation::SelectFrame
        );
        assert_eq!(
            frame_click_operation(true, false),
            EditorUiOperation::ToggleFrame
        );
        assert_eq!(
            frame_click_operation(false, true),
            EditorUiOperation::ExtendFrameRange
        );
        assert_eq!(
            frame_click_operation(true, true),
            EditorUiOperation::ExtendFrameRange
        );
    }

    #[test]
    fn numeric_inputs_handle_boundaries_and_overflow_without_panicking() {
        assert_eq!(parse_time_ms(" 42 ").unwrap(), TimeUs::new(42_000));
        assert!(parse_time_ms(&u64::MAX.to_string()).is_err());
        assert_eq!(parse_duration_us("1").unwrap(), DurationUs::new(1).unwrap());
        assert!(parse_duration_us("0").is_err());
        assert!(parse_duration_us("not a number").is_err());
    }

    #[test]
    fn time_range_inputs_reuse_millisecond_semantics_and_reject_overflow() {
        let mut state = EditorUiState {
            time_range_start_ms_input: " 12 ".to_owned(),
            time_range_end_ms_input: "34".to_owned(),
            ..EditorUiState::default()
        };
        assert_eq!(
            parse_time_range(&state).unwrap(),
            (TimeUs::new(12_000), TimeUs::new(34_000))
        );

        state.time_range_end_ms_input = u64::MAX.to_string();
        assert!(parse_time_range(&state).is_err());
        state.time_range_end_ms_input.clear();
        assert!(parse_time_range(&state).is_err());
    }

    #[test]
    fn advanced_timing_inputs_and_choice_labels_are_type_safe() {
        assert_eq!(parse_keep_every(" 2 ").unwrap(), 2);
        assert_eq!(
            parse_keep_every(&usize::MAX.to_string()).unwrap(),
            usize::MAX
        );
        assert!(parse_keep_every("0").is_err());
        assert!(parse_keep_every("1").is_err());
        assert!(parse_keep_every("2.5").is_err());

        for mode in [
            ReduceDelayMode::DontAdjust,
            ReduceDelayMode::Previous,
            ReduceDelayMode::Evenly,
        ] {
            assert!(!reduce_delay_label(mode).is_empty());
        }
        for scope in [YoyoScope::Selection, YoyoScope::EntireTimeline] {
            assert!(!yoyo_scope_label(scope).is_empty());
        }
        assert_eq!(parse_similarity_threshold("0").unwrap(), 0);
        assert_eq!(parse_similarity_threshold("100").unwrap(), 100);
        assert!(parse_similarity_threshold("101").is_err());
        assert!(parse_similarity_threshold("-1").is_err());
        for retention in [
            DuplicateFrameRetention::First,
            DuplicateFrameRetention::Last,
        ] {
            assert!(!duplicate_retention_label(retention).is_empty());
        }
        for mode in [
            DuplicateDelayMode::Keep,
            DuplicateDelayMode::Sum,
            DuplicateDelayMode::Average,
        ] {
            assert!(!duplicate_delay_label(mode).is_empty());
        }
    }

    #[test]
    fn every_transition_choice_builds_typed_settings_from_default_inputs() {
        let mut state = EditorUiState::default();
        for choice in [
            TransitionChoice::FadeToNext,
            TransitionChoice::FadeToColor,
            TransitionChoice::SlideLeft,
            TransitionChoice::SlideRight,
            TransitionChoice::SlideUp,
            TransitionChoice::SlideDown,
        ] {
            state.transition_choice = choice;
            let settings = build_transition_settings(&state).unwrap();
            assert_eq!(settings.duration, DurationUs::new(100_000).unwrap());
            assert_eq!(settings.steps, 5);
            assert!(!transition_choice_label(choice).is_empty());
            assert!(matches!(
                (choice, settings.kind),
                (TransitionChoice::FadeToNext, TransitionKind::FadeToNext)
                    | (
                        TransitionChoice::FadeToColor,
                        TransitionKind::FadeToColor {
                            color: Rgba {
                                red: 0,
                                green: 0,
                                blue: 0,
                                alpha: 255
                            }
                        }
                    )
                    | (
                        TransitionChoice::SlideLeft,
                        TransitionKind::Slide {
                            direction: SlideDirection::Left
                        }
                    )
                    | (
                        TransitionChoice::SlideRight,
                        TransitionKind::Slide {
                            direction: SlideDirection::Right
                        }
                    )
                    | (
                        TransitionChoice::SlideUp,
                        TransitionKind::Slide {
                            direction: SlideDirection::Up
                        }
                    )
                    | (
                        TransitionChoice::SlideDown,
                        TransitionKind::Slide {
                            direction: SlideDirection::Down
                        }
                    )
            ));
        }
    }

    #[test]
    fn transition_inputs_reject_zero_overflow_short_duration_and_bad_rgba() {
        let mut state = EditorUiState::default();
        for steps in ["0".to_owned(), (MAX_TRANSITION_STEPS + 1).to_string()] {
            state.transition_steps_input = steps;
            assert!(build_transition_settings(&state).is_err());
        }
        state.transition_steps_input = u32::from(u16::MAX).to_string();
        assert!(build_transition_settings(&state).is_err());
        state.transition_steps_input = "5".to_owned();
        state.transition_duration_us_input = "0".to_owned();
        assert!(build_transition_settings(&state).is_err());
        state.transition_duration_us_input = "4".to_owned();
        assert!(build_transition_settings(&state).is_err());
        state.transition_duration_us_input = u64::MAX.to_string();
        assert!(build_transition_settings(&state).is_ok());

        state.transition_choice = TransitionChoice::FadeToColor;
        state.transition_color_alpha_input = "256".to_owned();
        assert!(build_transition_settings(&state).is_err());
        state.transition_color_alpha_input = "0".to_owned();
        let settings = build_transition_settings(&state).unwrap();
        assert!(matches!(
            settings.kind,
            TransitionKind::FadeToColor {
                color: Rgba::TRANSPARENT
            }
        ));
    }

    #[test]
    fn every_effect_family_builds_from_typed_default_inputs() {
        let mut state = EditorUiState::default();
        for choice in [
            EffectChoice::Blur,
            EffectChoice::Pixelate,
            EffectChoice::Darken,
            EffectChoice::Lighten,
            EffectChoice::Border,
            EffectChoice::Shadow,
        ] {
            state.effect_choice = choice;
            let effect = build_effect(&state).unwrap();
            assert!(!effect_choice_label(choice).is_empty());
            assert!(matches!(
                (choice, effect),
                (EffectChoice::Blur, Effect::Blur { .. })
                    | (EffectChoice::Pixelate, Effect::Pixelate { .. })
                    | (EffectChoice::Darken, Effect::Darken { .. })
                    | (EffectChoice::Lighten, Effect::Lighten { .. })
                    | (EffectChoice::Border, Effect::Border { .. })
                    | (EffectChoice::Shadow, Effect::Shadow { .. })
            ));
        }
        assert_eq!(parse_effect_index(&state).unwrap(), 0);
    }

    #[test]
    fn effect_inputs_reject_invalid_ranges_parameters_colors_and_indices() {
        let mut state = EditorUiState {
            effect_choice: EffectChoice::Blur,
            effect_blur_radius_input: "0".to_owned(),
            ..EditorUiState::default()
        };
        assert!(build_effect(&state).is_err());
        state.effect_blur_radius_input = "257".to_owned();
        assert!(build_effect(&state).is_err());
        state.effect_blur_radius_input = "2".to_owned();
        state.effect_region_width_input = "0".to_owned();
        assert!(build_effect(&state).is_err());

        state = EditorUiState {
            effect_choice: EffectChoice::Pixelate,
            effect_pixel_block_input: "0".to_owned(),
            ..EditorUiState::default()
        };
        assert!(build_effect(&state).is_err());
        state.effect_choice = EffectChoice::Darken;
        state.effect_tone_percent_input = "101".to_owned();
        assert!(build_effect(&state).is_err());
        state.effect_choice = EffectChoice::Border;
        state.effect_border_top_input = "0".to_owned();
        state.effect_border_right_input = "0".to_owned();
        state.effect_border_bottom_input = "0".to_owned();
        state.effect_border_left_input = "0".to_owned();
        assert!(build_effect(&state).is_err());
        state.effect_border_top_input = "1".to_owned();
        state.effect_color_alpha_input = "0".to_owned();
        assert!(build_effect(&state).is_err());
        state.effect_color_alpha_input = "256".to_owned();
        assert!(build_effect(&state).is_err());
        state.effect_index_input = "0".to_owned();
        assert!(parse_effect_index(&state).is_err());
    }

    #[test]
    fn repair_success_action_retains_the_preserved_journal_path() {
        let path = std::path::Path::new("/tmp/project/journal.rejected-1.ndjson");
        let message = repair_journal_notice(Some(path));
        assert!(message.contains(path.to_string_lossy().as_ref()));
        let mut results = Vec::new();
        push_notice(
            &mut results,
            EditorUiOperation::RepairJournal,
            message.clone(),
        );
        assert_eq!(
            results,
            [Ok(EditorUiAction::Notice {
                operation: EditorUiOperation::RepairJournal,
                message,
            })]
        );
        assert!(repair_journal_notice(None).contains("already clean"));
    }

    #[test]
    fn statistics_duration_formatting_is_integer_exact() {
        assert_eq!(format_duration_us(0), "0.000000 s");
        assert_eq!(format_duration_us(1_234_567), "1.234567 s");
        assert_eq!(format_optional_duration(None), "None");
        assert_eq!(
            format_optional_duration(Some(u64::MAX)),
            format_duration_us(u64::MAX)
        );
    }

    #[test]
    fn crop_and_resize_inputs_validate_zero_overflow_and_u32_boundaries() {
        let mut state = EditorUiState {
            crop_x_input: "2".to_owned(),
            crop_y_input: "3".to_owned(),
            crop_width_input: "4".to_owned(),
            crop_height_input: "5".to_owned(),
            ..EditorUiState::default()
        };
        assert_eq!(
            parse_crop(&state).unwrap(),
            PhysicalRect::new(2, 3, 4, 5).unwrap()
        );

        state.crop_width_input = "0".to_owned();
        assert!(parse_crop(&state).is_err());
        state.crop_x_input = u32::MAX.to_string();
        state.crop_width_input = "1".to_owned();
        assert!(parse_crop(&state).is_err());
        state.crop_x_input = "not-a-number".to_owned();
        assert!(parse_crop(&state).is_err());

        state.resize_width_input = u32::MAX.to_string();
        state.resize_height_input = u32::MAX.to_string();
        assert_eq!(
            parse_output_size(&state).unwrap(),
            PhysicalSize::new(u32::MAX, u32::MAX).unwrap()
        );
        state.resize_height_input = "0".to_owned();
        assert!(parse_output_size(&state).is_err());
        state.resize_height_input = "4294967296".to_owned();
        assert!(parse_output_size(&state).is_err());
    }

    #[test]
    fn orientation_controls_map_to_their_project_operations() {
        for (control, expected) in [
            (
                OrientationControl::RotateLeft,
                EditorUiOperation::RotateLeft,
            ),
            (
                OrientationControl::RotateRight,
                EditorUiOperation::RotateRight,
            ),
            (
                OrientationControl::FlipHorizontal,
                EditorUiOperation::FlipHorizontal,
            ),
            (
                OrientationControl::FlipVertical,
                EditorUiOperation::FlipVertical,
            ),
        ] {
            assert_eq!(orientation_operation(control), expected);
        }
    }

    #[test]
    fn playback_clock_uses_monotonic_boundary_checks() {
        let started_at = Instant::now();
        let clock = PlaybackClock::new(
            FrameId::from_u128(1),
            started_at,
            DurationUs::new(2_000).unwrap(),
        );
        assert!(!clock.is_due(started_at + Duration::from_micros(1_999)));
        assert_eq!(
            clock.remaining(started_at + Duration::from_micros(1_999)),
            Duration::from_micros(1)
        );
        assert!(clock.is_due(started_at + Duration::from_millis(2)));
        assert!(
            clock
                .remaining(started_at + Duration::from_micros(2_001))
                .is_zero()
        );
    }

    fn playback_frames(durations: &[u64]) -> Vec<FrameClip> {
        durations
            .iter()
            .enumerate()
            .map(|(index, duration)| FrameClip {
                id: FrameId::from_u128(index as u128 + 1),
                asset_id: AssetId::from_digest([1; 32]),
                duration: DurationUs::new(*duration).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn playback_skips_late_frames_and_keeps_original_deadlines_after_wrapping() {
        let frames = playback_frames(&[10_000, 30_000, 20_000]);
        let start = Instant::now();
        let clock = PlaybackClock::new(frames[0].id, start, frames[0].duration);

        let (id, clock) = clock
            .advance(&frames, 0, start + Duration::from_millis(45), true)
            .unwrap();
        assert_eq!(id, frames[2].id);
        let clock = clock.unwrap();
        assert_eq!(clock.started_at, start + Duration::from_millis(40));
        assert_eq!(
            clock.remaining(start + Duration::from_millis(45)),
            Duration::from_millis(15)
        );

        let (id, clock) = clock
            .advance(&frames, 2, start + Duration::from_millis(60), true)
            .unwrap();
        assert_eq!(id, frames[0].id);
        let clock = clock.unwrap();
        assert_eq!(clock.started_at, start + Duration::from_millis(60));
        let (id, clock) = clock
            .advance(&frames, 0, start + Duration::from_millis(72), true)
            .unwrap();
        assert_eq!(id, frames[1].id);
        assert_eq!(clock.unwrap().started_at, start + Duration::from_millis(70));
    }

    #[test]
    fn playback_skips_millions_of_complete_loops_and_retains_submicrosecond_age() {
        let frames = playback_frames(&[1, 2, 3]);
        let start = Instant::now();
        let clock = PlaybackClock::new(frames[1].id, start, frames[1].duration);
        let now = start + Duration::from_nanos(6_000_000_003_500);
        let (id, clock) = clock.advance(&frames, 1, now, true).unwrap();
        assert_eq!(id, frames[2].id);
        assert_eq!(clock.unwrap().remaining(now), Duration::from_nanos(1_500));
    }

    #[test]
    fn one_shot_playback_holds_the_last_frame_after_a_long_stall() {
        let frames = playback_frames(&[10_000, 30_000, 20_000]);
        let start = Instant::now();
        let clock = PlaybackClock::new(frames[0].id, start, frames[0].duration);
        let (id, next) = clock
            .advance(&frames, 0, start + Duration::from_secs(2), false)
            .unwrap();
        assert_eq!(id, frames[2].id);
        assert!(next.is_none());
        assert!(EditorUiState::default().loop_preview);
    }

    #[test]
    fn single_frame_preview_repeats_or_stops_at_its_exact_boundary() {
        let frames = playback_frames(&[1_000]);
        let start = Instant::now();
        let now = start + Duration::from_millis(1);
        let clock = PlaybackClock::new(frames[0].id, start, frames[0].duration);
        let (id, next) = clock.advance(&frames, 0, now, true).unwrap();
        assert_eq!(id, frames[0].id);
        assert_eq!(next.unwrap().started_at, now);
        assert!(clock.advance(&frames, 0, now, false).unwrap().1.is_none());
        assert!(clock.advance(&[], 0, now, true).is_none());
    }

    #[test]
    fn fifty_thousand_frames_create_only_a_small_visible_widget_range() {
        let offset = (FILMSTRIP_ITEM_WIDTH + 8.0) * 25_000.0;
        let visible = visible_widget_range(50_000, offset, 1_200.0).unwrap();
        assert!(visible.contains(&25_000));
        assert!(visible.len() < 20);
        assert!(visible.end <= 50_000);

        let tail = visible_widget_range(50_000, f64::MAX / 2.0, 1.0).unwrap();
        assert_eq!(tail, 49_997..50_000);
    }

    #[test]
    fn filmstrip_height_stays_one_card_tall_with_many_visible_frames() {
        use gif_from_screen_application::{
            BlankAnimationProjectOptions, create_blank_animation_project,
        };
        use gif_from_screen_domain::{EditCommand, ProjectId, UnixTimeMs};

        let directory = tempfile::tempdir().unwrap();
        let mut project = create_blank_animation_project(
            directory.path().join("layout.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(1),
                frame_id: FrameId::from_u128(1),
                app_version: "layout-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(1, 1).unwrap(),
                background: Rgba {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 0,
                },
                frame_duration: DurationUs::new(50_000).unwrap(),
                frame_limit_bytes: 4,
            },
        )
        .unwrap();
        let first = project.manifest().timeline.frames[0].clone();
        let frames = (2..=36)
            .map(|id| FrameClip {
                id: FrameId::from_u128(id),
                ..first.clone()
            })
            .collect();
        project
            .commit(EditCommand::InsertFrames { index: 1, frames })
            .unwrap();
        let mut workspace =
            crate::editor_workspace::EditorWorkspace::from_active(project, 10).unwrap();
        let mut state = EditorUiState::default();
        let context = eframe::egui::Context::default();
        let input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(1_040.0, 760.0),
            )),
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            eframe::egui::CentralPanel::default().show(context, |ui| {
                ui.spacing_mut().item_spacing = eframe::egui::vec2(8.0, 8.0);
                let top = ui.cursor().top();
                super::show_virtual_filmstrip(
                    ui,
                    &mut workspace,
                    &mut state,
                    Instant::now(),
                    &mut Vec::new(),
                );
                let height = ui.cursor().top() - top;
                assert!(
                    height <= 104.0,
                    "filmstrip occupied {height} points for 78-point cards"
                );
            });
        });
    }

    #[test]
    fn f64_to_egui_boundary_rejects_unrepresentable_values() {
        assert!((to_ui_points(12.5).unwrap() - 12.5_f32).abs() <= f32::EPSILON);
        assert!(to_ui_points(-1.0).is_err());
        assert!(to_ui_points(f64::NAN).is_err());
        assert!(to_ui_points(f64::MAX).is_err());
    }
}
