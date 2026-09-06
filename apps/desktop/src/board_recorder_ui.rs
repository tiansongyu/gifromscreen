//! Interactive drawing canvas with a nonblocking durable recording worker.

use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use gif_from_screen_application::{
    BoardBrush, BoardCanvas, BoardPoint, LiveFrameSubmission, LiveRecordingOptions,
    LiveRecordingOutcome, LiveRgbaRecorder,
};
use gif_from_screen_domain::{PhysicalSize, ProjectId, Rgba, SourceProvenance, UnixTimeMs};
use gif_from_screen_project::ActiveProject;
use uuid::Uuid;

#[derive(Clone, Copy, Default, PartialEq)]
enum BrushChoice {
    #[default]
    Pen,
    Highlighter,
    Eraser,
}
#[derive(Clone, Copy, Default, PartialEq)]
enum BoardCadence {
    #[default]
    Fps,
    Stroke,
}
#[derive(Clone, Copy, Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Recording,
    Paused,
    Stopping(u64),
    Discarding,
}

struct ActiveClock {
    accumulated: Duration,
    resumed: Option<Instant>,
}

impl ActiveClock {
    fn new(now: Instant) -> Self {
        Self {
            accumulated: Duration::ZERO,
            resumed: Some(now),
        }
    }
    fn elapsed_us(&self, now: Instant) -> u64 {
        let elapsed = self.accumulated.saturating_add(
            self.resumed
                .map_or(Duration::ZERO, |start| now.saturating_duration_since(start)),
        );
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
    }
    fn pause(&mut self, now: Instant) {
        if let Some(start) = self.resumed.take() {
            self.accumulated = self
                .accumulated
                .saturating_add(now.saturating_duration_since(start));
        }
    }
    fn resume(&mut self, now: Instant) {
        if self.resumed.is_none() {
            self.resumed = Some(now);
        }
    }
}

pub(crate) struct BoardRecorderTool {
    width: u32,
    height: u32,
    transparent: bool,
    background: [u8; 4],
    brush: BrushChoice,
    color: [u8; 4],
    brush_width: u16,
    cadence: BoardCadence,
    fps: u32,
    target: String,
    canvas: Option<BoardCanvas>,
    texture: Option<egui::TextureHandle>,
    texture_revision: Option<u64>,
    writer: Option<LiveRgbaRecorder>,
    clock: Option<ActiveClock>,
    phase: Phase,
    next_frame_us: u64,
    last_submitted_us: Option<u64>,
    last_submitted_revision: u64,
    pending: Option<(u64, Vec<u8>)>,
    skipped: usize,
    previous_point: Option<BoardPoint>,
    notice: Option<String>,
}

impl Default for BoardRecorderTool {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            transparent: false,
            background: [255; 4],
            brush: BrushChoice::Pen,
            color: [30, 40, 60, 255],
            brush_width: 5,
            cadence: BoardCadence::Fps,
            fps: 10,
            target: String::new(),
            canvas: None,
            texture: None,
            texture_revision: None,
            writer: None,
            clock: None,
            phase: Phase::Idle,
            next_frame_us: 0,
            last_submitted_us: None,
            last_submitted_revision: 0,
            pending: None,
            skipped: 0,
            previous_point: None,
            notice: None,
        }
    }
}

impl BoardRecorderTool {
    pub(crate) fn is_active(&self) -> bool {
        self.writer
            .as_ref()
            .is_some_and(LiveRgbaRecorder::is_active)
    }

    /// Explicit discard, distinct from application shutdown.
    pub(crate) fn cancel(&mut self) {
        if let Some(writer) = &self.writer {
            writer.discard();
            self.phase = Phase::Discarding;
            self.pending = None;
        }
    }

    /// Ordinary window close saves the current board and excludes paused time.
    pub(crate) fn shutdown(&mut self) {
        self.stop(Instant::now());
    }

    pub(crate) fn poll(&mut self) -> Option<Result<ActiveProject, String>> {
        self.advance(Instant::now());
        let result = self.writer.as_mut()?.poll()?;
        let progress = self.writer.as_ref().map(LiveRgbaRecorder::progress);
        self.writer = None;
        self.phase = Phase::Idle;
        self.pending = None;
        self.previous_point = None;
        if let Some(canvas) = &mut self.canvas {
            canvas.end();
        }
        match result {
            Ok(LiveRecordingOutcome::Saved(project)) => {
                self.notice = Some(if progress.is_some_and(|progress| progress.limit_reached) {
                    "Recording reached its frame/storage limit and was saved.".to_owned()
                } else {
                    "Board recording saved. Open it in the editor.".to_owned()
                });
                Some(Ok(project))
            }
            Ok(LiveRecordingOutcome::Discarded) => {
                self.notice = Some(
                    "Board recording discarded; only its newly created project was removed."
                        .to_owned(),
                );
                None
            }
            Err(error) => {
                self.notice = Some(error.clone());
                Some(Err(error))
            }
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        ui.heading("Drawing board");
        ui.label("Record a sketch, explanation or step-by-step drawing as a GIF.");
        self.show_configuration(ui);
        self.show_controls(ui);
        ui.separator();
        ui.add_enabled_ui(
            matches!(self.phase, Phase::Idle | Phase::Recording) && self.pending.is_none(),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.brush, BrushChoice::Pen, "Pen");
                    ui.selectable_value(&mut self.brush, BrushChoice::Highlighter, "Highlighter");
                    ui.selectable_value(&mut self.brush, BrushChoice::Eraser, "Eraser");
                    ui.add(
                        egui::DragValue::new(&mut self.brush_width)
                            .range(1..=256)
                            .suffix(" px"),
                    );
                    ui.color_edit_button_srgba_unmultiplied(&mut self.color);
                });
            },
        );
        ui.weak("Round hard-edged brush · highlighter has 25% maximum opacity per stroke · eraser restores the canvas background.");
        if self.canvas.is_none()
            && let Err(error) = self.reset_canvas()
        {
            self.notice = Some(error);
        }
        self.show_canvas(ui);
        if self.is_active() {
            let elapsed = self
                .clock
                .as_ref()
                .map_or(0, |clock| clock.elapsed_us(Instant::now()));
            let progress = self
                .writer
                .as_ref()
                .map(LiveRgbaRecorder::progress)
                .unwrap_or_default();
            ui.label(format!(
                "{}.{:02} s active · {} saved frames · {} skipped FPS samples",
                elapsed / 1_000_000,
                elapsed % 1_000_000 / 10_000,
                progress.frames,
                self.skipped
            ));
            if self.pending.is_some() {
                ui.weak("Saving this drawing step… Drawing resumes when the bounded writer queue is ready.");
            }
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
    }

    fn show_configuration(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.is_active(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::DragValue::new(&mut self.width).range(1..=2048).prefix("Width "));
                ui.add(egui::DragValue::new(&mut self.height).range(1..=2048).prefix("Height "));
                ui.checkbox(&mut self.transparent, "Transparent background");
                ui.add_enabled_ui(!self.transparent, |ui| { ui.color_edit_button_srgba_unmultiplied(&mut self.background); });
                if ui.button("Reset canvas").clicked() && let Err(error) = self.reset_canvas() { self.notice = Some(error); }
            });
            ui.weak("Reset canvas applies size/background changes and clears the current unrecorded drawing.");
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.cadence, BoardCadence::Fps, "Automatic");
                ui.add_enabled(self.cadence == BoardCadence::Fps, egui::DragValue::new(&mut self.fps).range(1..=60).suffix(" fps"));
                ui.selectable_value(&mut self.cadence, BoardCadence::Stroke, "Each completed stroke");
            });
            ui.horizontal(|ui| { ui.label("New project"); ui.add(egui::TextEdit::singleline(&mut self.target).desired_width(430.0).hint_text("Leave empty for a unique board project in the current folder")); });
        });
    }

    fn show_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| match self.phase {
            Phase::Idle => {
                if ui.button("Start recording").clicked()
                    && let Err(error) = self.start(Instant::now())
                {
                    self.notice = Some(error);
                }
            }
            Phase::Recording | Phase::Paused => {
                if ui
                    .button(if self.phase == Phase::Recording {
                        "Pause"
                    } else {
                        "Continue"
                    })
                    .clicked()
                {
                    self.toggle_pause(Instant::now());
                }
                if ui.button("Stop and edit").clicked() {
                    self.stop(Instant::now());
                }
                if ui.button("Discard recording").clicked() {
                    self.cancel();
                }
            }
            Phase::Stopping(_) => {
                ui.spinner();
                ui.label("Saving recording…");
            }
            Phase::Discarding => {
                ui.spinner();
                ui.label("Discarding new project…");
            }
        });
    }

    fn reset_canvas(&mut self) -> Result<(), String> {
        let background = if self.transparent {
            Rgba::TRANSPARENT
        } else {
            rgba(self.background)
        };
        self.canvas = Some(BoardCanvas::new(
            PhysicalSize::new(self.width, self.height).map_err(|error| error.to_string())?,
            background,
        )?);
        self.texture = None;
        self.texture_revision = None;
        Ok(())
    }

    fn start(&mut self, now: Instant) -> Result<(), String> {
        if self.is_active() {
            return Err("A board recording is already active".to_owned());
        }
        if self.canvas.is_none() {
            self.reset_canvas()?;
        }
        let canvas = self
            .canvas
            .as_ref()
            .ok_or_else(|| "Create the board canvas first".to_owned())?;
        if self.fps == 0 || self.fps > 60 {
            return Err("Board frame rate must be 1..60 fps".to_owned());
        }
        let uuid = Uuid::new_v4();
        let project_path = if self.target.trim().is_empty() {
            PathBuf::from(format!("board-{uuid}.gfsproj"))
        } else {
            PathBuf::from(self.target.trim())
        };
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        let writer = LiveRgbaRecorder::start(LiveRecordingOptions {
            project_path,
            canvas: canvas.size(),
            project_id: ProjectId::from_u128(uuid.as_u128()),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at: UnixTimeMs::new(
                i64::try_from(created_at.as_millis()).map_err(|error| error.to_string())?,
            ),
            provenance: SourceProvenance::Board,
            provisional_duration_us: 1_000_000 / u64::from(self.fps),
            max_frames: 100_000,
            max_frame_bytes_total: 4 * 1024 * 1024 * 1024,
        })?;
        writer.try_frame(0, canvas.pixels().to_vec())?;
        self.writer = Some(writer);
        self.clock = Some(ActiveClock::new(now));
        self.phase = Phase::Recording;
        self.last_submitted_us = Some(0);
        self.last_submitted_revision = canvas.revision();
        self.next_frame_us = 1_000_000 / u64::from(self.fps);
        self.skipped = 0;
        self.pending = None;
        self.notice = Some("Recording to a new project. Pause time is excluded; Stop saves, Discard removes this recording.".to_owned());
        Ok(())
    }

    fn toggle_pause(&mut self, now: Instant) {
        if self.phase == Phase::Recording {
            self.finish_stroke(now);
            if let Some(clock) = &mut self.clock {
                clock.pause(now);
            }
            self.phase = Phase::Paused;
        } else if self.phase == Phase::Paused {
            if let Some(clock) = &mut self.clock {
                clock.resume(now);
            }
            self.phase = Phase::Recording;
        }
    }

    fn stop(&mut self, now: Instant) {
        if !matches!(self.phase, Phase::Recording | Phase::Paused) {
            return;
        }
        self.finish_stroke(now);
        let at = self.clock.as_ref().map_or(1, |clock| clock.elapsed_us(now));
        if let Some(clock) = &mut self.clock {
            clock.pause(now);
        }
        self.phase = Phase::Stopping(at);
        if self
            .canvas
            .as_ref()
            .is_some_and(|canvas| canvas.revision() != self.last_submitted_revision)
            && self.pending.is_none()
        {
            self.queue_snapshot(at.saturating_sub(1));
        }
        self.advance(now);
    }

    fn advance(&mut self, now: Instant) {
        let Some(writer) = &self.writer else {
            return;
        };
        if let Some((at, pixels)) = self.pending.take() {
            match writer.try_frame_retaining(at, pixels) {
                Ok((LiveFrameSubmission::Accepted, _)) => {
                    self.last_submitted_us = Some(at);
                    self.last_submitted_revision =
                        self.canvas.as_ref().map_or(0, BoardCanvas::revision);
                }
                Ok((LiveFrameSubmission::Backpressure, pixels)) => {
                    self.pending = Some((at, pixels));
                }
                Ok((LiveFrameSubmission::Finishing, _)) => {}
                Err(error) => {
                    self.notice = Some(error);
                    writer.stop_at(at);
                    self.phase = Phase::Stopping(at);
                }
            }
        }
        if let Phase::Stopping(at) = self.phase {
            if self.pending.is_none() {
                writer.stop_at(at);
            }
            return;
        }
        if self.phase == Phase::Recording
            && self.cadence == BoardCadence::Fps
            && self.pending.is_none()
        {
            let at = self.clock.as_ref().map_or(0, |clock| clock.elapsed_us(now));
            if at >= self.next_frame_us {
                self.next_frame_us = at.saturating_add(1_000_000 / u64::from(self.fps));
                self.submit_fps(at);
            }
        }
    }

    fn submit_fps(&mut self, at: u64) {
        let (Some(writer), Some(canvas)) = (&self.writer, &self.canvas) else {
            return;
        };
        if self.last_submitted_us.is_some_and(|last| at <= last) {
            return;
        }
        match writer.try_frame(at, canvas.pixels().to_vec()) {
            Ok(LiveFrameSubmission::Accepted) => {
                self.last_submitted_us = Some(at);
                self.last_submitted_revision = canvas.revision();
            }
            Ok(LiveFrameSubmission::Backpressure) => {
                self.skipped += 1;
            }
            Ok(LiveFrameSubmission::Finishing) => {}
            Err(error) => {
                self.notice = Some(error);
                writer.stop_at(at);
                self.phase = Phase::Stopping(at);
            }
        }
    }

    fn queue_snapshot(&mut self, at: u64) {
        if let Some(canvas) = &self.canvas {
            let at = at.max(self.last_submitted_us.unwrap_or(0).saturating_add(1));
            self.pending = Some((at, canvas.pixels().to_vec()));
        }
    }

    fn finish_stroke(&mut self, now: Instant) {
        let ended = self.canvas.as_mut().is_some_and(BoardCanvas::end);
        self.previous_point = None;
        if ended && self.phase == Phase::Recording && self.cadence == BoardCadence::Stroke {
            let at = self.clock.as_ref().map_or(1, |clock| clock.elapsed_us(now));
            self.queue_snapshot(at);
        }
    }

    fn show_canvas(&mut self, ui: &mut egui::Ui) {
        let Some(canvas) = &self.canvas else {
            return;
        };
        if self.texture_revision != Some(canvas.revision()) {
            let size = [
                canvas.size().width.get() as usize,
                canvas.size().height.get() as usize,
            ];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, canvas.pixels());
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                self.texture = Some(ui.ctx().load_texture(
                    "drawing-board",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
            self.texture_revision = Some(canvas.revision());
        }
        let Some(texture) = &self.texture else {
            return;
        };
        let native = egui::vec2(
            f32::from(u16::try_from(canvas.size().width.get()).unwrap_or(2048)),
            f32::from(u16::try_from(canvas.size().height.get()).unwrap_or(2048)),
        );
        let scale = (ui.available_width() / native.x)
            .min((ui.available_height() - 70.0).max(100.0) / native.y)
            .min(1.0);
        let display = native * scale;
        let response = ui.add(
            egui::Image::new((texture.id(), display))
                .sense(egui::Sense::click_and_drag())
                .bg_fill(egui::Color32::from_gray(210)),
        );
        let size = canvas.size();
        let editable =
            matches!(self.phase, Phase::Idle | Phase::Recording) && self.pending.is_none();
        if editable && let Some(position) = response.interact_pointer_pos() {
            let point = map_point(response.rect, size, position);
            if response.clicked_by(egui::PointerButton::Primary) {
                if self.canvas.as_ref().is_some_and(BoardCanvas::is_drawing) {
                    if self.previous_point != Some(point) {
                        self.extend_stroke(point);
                    }
                } else {
                    self.begin_stroke(point);
                }
                self.finish_stroke(Instant::now());
            } else if response.hovered() && ui.input(|input| input.pointer.primary_pressed()) {
                self.begin_stroke(point);
            } else if response.dragged_by(egui::PointerButton::Primary)
                && self.previous_point != Some(point)
            {
                self.extend_stroke(point);
            }
        }
        if ui.input(|input| input.pointer.primary_released()) {
            self.finish_stroke(Instant::now());
        }
        if self
            .canvas
            .as_ref()
            .is_some_and(|canvas| self.texture_revision != Some(canvas.revision()))
        {
            ui.ctx().request_repaint();
        }
    }

    fn begin_stroke(&mut self, point: BoardPoint) {
        let brush = match self.brush {
            BrushChoice::Pen => BoardBrush::Pen(rgba(self.color)),
            BrushChoice::Highlighter => BoardBrush::Highlighter(rgba(self.color)),
            BrushChoice::Eraser => BoardBrush::Eraser,
        };
        if let Some(canvas) = &mut self.canvas {
            match canvas.begin(point, brush, self.brush_width) {
                Ok(()) => self.previous_point = Some(point),
                Err(error) => self.notice = Some(error),
            }
        }
    }

    fn extend_stroke(&mut self, point: BoardPoint) {
        if let Some(canvas) = &mut self.canvas
            && canvas.is_drawing()
        {
            match canvas.extend(point) {
                Ok(()) => self.previous_point = Some(point),
                Err(error) => {
                    self.notice = Some(error);
                    self.finish_stroke(Instant::now());
                }
            }
        }
    }
}

fn rgba(color: [u8; 4]) -> Rgba {
    Rgba {
        red: color[0],
        green: color[1],
        blue: color[2],
        alpha: color[3],
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "coordinates are clamped to a board of at most 2048 pixels per edge"
)]
fn map_point(rect: egui::Rect, size: PhysicalSize, position: egui::Pos2) -> BoardPoint {
    BoardPoint {
        x: ((((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * size.width.get() as f32)
            .floor()
            .max(0.0) as u32)
            .min(size.width.get().saturating_sub(1)),
        y:
            ((((position.y - rect.top()) / rect.height()).clamp(0.0, 1.0)
                * size.height.get() as f32)
                .floor()
                .max(0.0) as u32)
                .min(size.height.get().saturating_sub(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn tool(root: &std::path::Path, cadence: BoardCadence) -> BoardRecorderTool {
        let mut tool = BoardRecorderTool {
            width: 32,
            height: 32,
            target: root.join("board.gfsproj").to_string_lossy().into_owned(),
            cadence,
            ..BoardRecorderTool::default()
        };
        tool.reset_canvas().unwrap();
        tool
    }

    fn saved(tool: &mut BoardRecorderTool) -> ActiveProject {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = tool.poll() {
                return result.unwrap();
            }
            assert!(Instant::now() < deadline, "board writer did not finish");
            thread::yield_now();
        }
    }

    fn pending_written(tool: &mut BoardRecorderTool, now: Instant, frames: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            tool.advance(now);
            if tool.pending.is_none() && tool.writer.as_ref().unwrap().progress().frames >= frames {
                break;
            }
            assert!(Instant::now() < deadline, "board snapshot was not written");
            thread::yield_now();
        }
    }

    #[test]
    fn activity_clock_excludes_all_paused_intervals_and_handles_duplicate_controls() {
        let start = Instant::now();
        let mut clock = ActiveClock::new(start);
        clock.pause(start + Duration::from_millis(100));
        clock.pause(start + Duration::from_secs(2));
        assert_eq!(clock.elapsed_us(start + Duration::from_secs(5)), 100_000);
        clock.resume(start + Duration::from_secs(5));
        clock.resume(start + Duration::from_secs(6));
        assert_eq!(
            clock.elapsed_us(start + Duration::from_millis(5100)),
            200_000
        );
    }

    #[test]
    fn completed_strokes_preserve_real_activity_time_and_pause_exclusion() {
        let directory = tempfile::tempdir().unwrap();
        let mut tool = tool(directory.path(), BoardCadence::Stroke);
        let start = Instant::now();
        tool.start(start).unwrap();
        tool.begin_stroke(BoardPoint { x: 2, y: 2 });
        tool.extend_stroke(BoardPoint { x: 20, y: 2 });
        tool.finish_stroke(start + Duration::from_millis(100));
        pending_written(&mut tool, start + Duration::from_millis(100), 2);
        tool.toggle_pause(start + Duration::from_millis(200));
        tool.advance(start + Duration::from_secs(1));
        assert_eq!(tool.writer.as_ref().unwrap().progress().frames, 2);
        tool.toggle_pause(start + Duration::from_secs(1));
        tool.begin_stroke(BoardPoint { x: 2, y: 20 });
        tool.extend_stroke(BoardPoint { x: 20, y: 20 });
        tool.finish_stroke(start + Duration::from_millis(1100));
        pending_written(&mut tool, start + Duration::from_millis(1100), 3);
        tool.stop(start + Duration::from_millis(1200));
        let project = saved(&mut tool);
        assert_eq!(
            project.manifest().source_provenance,
            [SourceProvenance::Board]
        );
        assert_eq!(
            project
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [100_000, 200_000, 100_000]
        );
    }

    #[test]
    fn automatic_sampling_uses_actual_clock_without_paused_frames() {
        let directory = tempfile::tempdir().unwrap();
        let mut tool = tool(directory.path(), BoardCadence::Fps);
        let start = Instant::now();
        tool.start(start).unwrap();
        pending_written(&mut tool, start, 1);
        pending_written(&mut tool, start + Duration::from_millis(100), 2);
        pending_written(&mut tool, start + Duration::from_millis(200), 3);
        tool.toggle_pause(start + Duration::from_millis(250));
        tool.advance(start + Duration::from_secs(1));
        assert_eq!(tool.writer.as_ref().unwrap().progress().frames, 3);
        tool.toggle_pause(start + Duration::from_secs(1));
        pending_written(&mut tool, start + Duration::from_millis(1050), 4);
        tool.stop(start + Duration::from_millis(1100));
        let project = saved(&mut tool);
        assert_eq!(
            project
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [100_000, 100_000, 100_000, 50_000]
        );
    }

    #[test]
    fn normal_shutdown_saves_while_explicit_discard_removes_the_new_project() {
        let directory = tempfile::tempdir().unwrap();
        let mut tool = tool(directory.path(), BoardCadence::Stroke);
        tool.start(Instant::now()).unwrap();
        tool.shutdown();
        let project = saved(&mut tool);
        assert!(project.layout().manifest.exists());
        drop(project);
        tool.target = directory
            .path()
            .join("discard.gfsproj")
            .to_string_lossy()
            .into_owned();
        tool.start(Instant::now()).unwrap();
        tool.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tool.is_active() {
            assert!(tool.poll().is_none());
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(!directory.path().join("discard.gfsproj").exists());
    }

    #[test]
    fn egui_pointer_drag_paints_pixels_and_paused_input_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let mut tool = tool(directory.path(), BoardCadence::Stroke);
        let context = egui::Context::default();
        let initial = tool.canvas.as_ref().unwrap().pixels().to_vec();
        let show = |tool: &mut BoardRecorderTool, events| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 200.0),
                    )),
                    events,
                    ..egui::RawInput::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| tool.show_canvas(ui));
                },
            );
        };
        show(&mut tool, vec![]);
        let start = egui::pos2(12.0, 12.0);
        let end = egui::pos2(32.0, 32.0);
        show(
            &mut tool,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        show(&mut tool, vec![egui::Event::PointerMoved(end)]);
        show(
            &mut tool,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_ne!(tool.canvas.as_ref().unwrap().pixels(), initial);
        assert!(!tool.canvas.as_ref().unwrap().is_drawing());
        let painted = tool.canvas.as_ref().unwrap().pixels().to_vec();
        tool.phase = Phase::Paused;
        show(
            &mut tool,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        show(&mut tool, vec![egui::Event::PointerMoved(end)]);
        show(
            &mut tool,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(tool.canvas.as_ref().unwrap().pixels(), painted);
        tool.phase = Phase::Idle;
        tool.reset_canvas().unwrap();
        show(&mut tool, vec![]);
        show(
            &mut tool,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(
            tool.canvas.as_ref().unwrap().pixels() != initial,
            "a quick click must paint a dot"
        );
    }
}
