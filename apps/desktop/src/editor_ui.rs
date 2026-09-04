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
    DurationUs, EdgeWidths, Effect, FrameId, PhysicalRect, PhysicalSize, Rgba, TimeUs,
};
use gif_from_screen_editor::{
    DuplicateDelayMode, DuplicateFrameRetention, MAX_FRAME_EFFECT_BLUR_RADIUS, ReduceDelayMode,
    VirtualFilmstripError, VirtualFilmstripLayout, YoyoScope, parse_frame_expression,
};

use crate::editor_workspace::{EditorWorkspace, EditorWorkspaceError};

const FILMSTRIP_ITEM_WIDTH: f64 = 112.0;
const FILMSTRIP_ITEM_GAP: f64 = 8.0;
const FILMSTRIP_ITEM_HEIGHT: f32 = 78.0;
const FILMSTRIP_OVERSCAN: usize = 3;

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

/// Ephemeral editor controls and playback state retained between egui frames.
#[derive(Debug)]
pub(crate) struct EditorUiState {
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
    /// Monotonic playback clock when playback is active.
    pub(crate) playback: Option<PlaybackClock>,
    filmstrip_scroll_offset: f64,
    reveal_current_frame: bool,
}

impl Default for EditorUiState {
    fn default() -> Self {
        Self {
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
            crop_x_input: "0".into(),
            crop_y_input: "0".into(),
            crop_width_input: "1".into(),
            crop_height_input: "1".into(),
            resize_width_input: "1".into(),
            resize_height_input: "1".into(),
            playback: None,
            filmstrip_scroll_offset: 0.0,
            reveal_current_frame: false,
        }
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

/// Draws the first interactive editor surface and returns all actions attempted this frame.
///
/// The filmstrip is virtualized: its `ScrollArea` creates widgets only for the range returned by
/// [`VirtualFilmstripLayout::visible_range`]. Frame pixels and export controls are intentionally
/// deferred.
pub(crate) fn show_editor_ui(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
) -> Vec<EditorUiResult> {
    let now = Instant::now();
    let mut results = Vec::new();
    advance_playback(ui.ctx(), workspace, state, now, &mut results);

    show_editor_summary(ui, workspace);
    show_project_storage_toolbar(ui, workspace, &mut results);
    show_editor_statistics(ui, workspace, &mut results);
    ui.separator();
    show_navigation_toolbar(ui, workspace, state, now, &mut results);
    show_selection_toolbar(ui, workspace, state, now, &mut results);
    show_time_range_toolbar(ui, workspace, state, now, &mut results);
    show_edit_toolbar(ui, workspace, state, now, &mut results);
    show_advanced_timing_toolbar(ui, workspace, state, now, &mut results);
    show_transform_toolbar(ui, workspace, state, now, &mut results);
    show_effect_toolbar(ui, workspace, state, now, &mut results);
    ui.separator();
    show_virtual_filmstrip(ui, workspace, state, now, &mut results);

    schedule_playback_repaint(ui.ctx(), state, now);
    results
}

fn show_editor_summary(ui: &mut egui::Ui, workspace: &EditorWorkspace) {
    ui.horizontal_wrapped(|ui| {
        ui.heading("Editor");
        ui.label(format!(
            "{} frames",
            workspace.manifest().timeline.frames.len()
        ));
        ui.label(format!("{} selected", workspace.selection().len()));
        ui.label(format!("Clipboard: {} frame(s)", workspace.clipboard_len()));
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

#[allow(
    clippy::too_many_lines,
    reason = "keeping the edit buttons together makes their workspace action mapping auditable"
)]
fn show_edit_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    now: Instant,
    results: &mut Vec<EditorUiResult>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Edit");
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
        .auto_shrink([false, false])
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
                let marker = if is_current { "\nCurrent" } else { "" };
                let label =
                    egui::RichText::new(format!("Frame {}\n{} µs{marker}", index + 1, duration_us));
                let mut button = egui::Button::new(label)
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
                let response = ui
                    .push_id(("editor-frame-card", index), |ui| ui.put(rect, button))
                    .inner;
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
    if index.checked_add(1) == Some(workspace.manifest().timeline.frames.len()) {
        state.playback = None;
        results.push(Ok(EditorUiAction::Playback { playing: false }));
        return;
    }

    match workspace.select_next() {
        Ok(_) => {
            results.push(Ok(EditorUiAction::Selection(
                EditorUiOperation::PlaybackStep,
            )));
            state.reveal_current_frame = true;
            state.playback = playback_clock_for_current(workspace, now);
            schedule_playback_repaint(context, state, now);
        }
        Err(error) => {
            state.playback = None;
            push_failure(results, EditorUiOperation::PlaybackStep, error);
            results.push(Ok(EditorUiAction::Playback { playing: false }));
        }
    }
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

    use gif_from_screen_domain::{DurationUs, Effect, FrameId, PhysicalRect, PhysicalSize, TimeUs};
    use gif_from_screen_editor::{
        DuplicateDelayMode, DuplicateFrameRetention, ReduceDelayMode, YoyoScope,
    };

    use super::{
        EditorUiAction, EditorUiOperation, EditorUiState, EffectChoice, FILMSTRIP_ITEM_WIDTH,
        OrientationControl, PlaybackClock, build_effect, duplicate_delay_label,
        duplicate_retention_label, effect_choice_label, format_duration_us,
        format_optional_duration, frame_click_operation, orientation_operation, parse_crop,
        parse_duration_us, parse_effect_index, parse_keep_every, parse_output_size,
        parse_similarity_threshold, parse_time_ms, parse_time_range, push_notice,
        reduce_delay_label, repair_journal_notice, to_ui_points, visible_widget_range,
        yoyo_scope_label,
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
        assert_eq!(state.crop_x_input, "0");
        assert_eq!(state.crop_y_input, "0");
        assert_eq!(state.crop_width_input, "1");
        assert_eq!(state.crop_height_input, "1");
        assert_eq!(state.resize_width_input, "1");
        assert_eq!(state.resize_height_input, "1");
        assert!(state.playback.is_none());
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
    fn f64_to_egui_boundary_rejects_unrepresentable_values() {
        assert!((to_ui_points(12.5).unwrap() - 12.5_f32).abs() <= f32::EPSILON);
        assert!(to_ui_points(-1.0).is_err());
        assert!(to_ui_points(f64::NAN).is_err());
        assert!(to_ui_points(f64::MAX).is_err());
    }
}
