#![forbid(unsafe_code)]

//! Desktop entry point for the Linux-first `GifFromScreen` application.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use eframe::egui;
use gif_from_screen_capture::{
    CaptureBackend, CaptureCadence, CaptureRequest, CaptureSource, CaptureSourceId,
    CaptureSourceKind, CaptureTarget, CapturedFrame, CursorCaptureMode, PhysicalRect, PixelFormat,
};
use gif_from_screen_capture_linux::X11CaptureBackend;
use gif_from_screen_gif::{CancellationFlag, EncodeOptions};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, RecordToGifOptions, RecordToGifReport, WorkflowProgress,
    record_to_gif,
};

const APP_NAME: &str = "GifFromScreen";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AppView {
    #[default]
    Landing,
    ScreenRecorder,
}

#[derive(Clone, Debug)]
struct RecordingSettings {
    output: String,
    duration_ms: u64,
    fps: u32,
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
            duration_ms: 3_000,
            fps: 10,
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
    Finished(Result<RecordToGifReport, String>),
}

struct RecordingJob {
    receiver: Receiver<JobMessage>,
    cancellation: CancellationFlag,
}

struct RegionPicker {
    texture: egui::TextureHandle,
    source_width: u32,
    source_height: u32,
    drag_start: Option<egui::Pos2>,
    drag_current: Option<egui::Pos2>,
    selection: Option<PhysicalRect>,
}

struct GifFromScreenApp {
    view: AppView,
    notice: Option<String>,
    settings: RecordingSettings,
    sources: Vec<CaptureSource>,
    selected_source: usize,
    region_picker: Option<RegionPicker>,
    job: Option<RecordingJob>,
    progress: Option<WorkflowProgress>,
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
            job: None,
            progress: None,
        }
    }
}

impl Drop for GifFromScreenApp {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancellation.cancel();
        }
    }
}

impl eframe::App for GifFromScreenApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_job_messages();
        if self.job.is_some() {
            context.request_repaint_after(Duration::from_millis(33));
        }

        egui::TopBottomPanel::top("app_header").show(context, |ui| {
            ui.horizontal(|ui| {
                if self.view != AppView::Landing && ui.button("Back").clicked() {
                    self.view = AppView::Landing;
                }
                ui.heading(APP_NAME);
                ui.separator();
                ui.label("Linux X11 preview");
            });
        });

        egui::CentralPanel::default().show(context, |ui| match self.view {
            AppView::Landing => self.show_landing(ui),
            AppView::ScreenRecorder => self.show_screen_recorder(ui),
        });
    }
}

impl GifFromScreenApp {
    fn show_landing(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.heading("Create an animated GIF");
            ui.label("Capture, edit frame by frame, and export locally.");
            ui.add_space(28.0);

            ui.columns(2, |columns| {
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
                    "Open or import",
                    "Open a project, GIF, image sequence, or video.",
                    false,
                ) {
                    self.notice =
                        Some("Import is scheduled after the first recorder slice.".into());
                }
            });

            ui.add_space(12.0);
            ui.columns(2, |columns| {
                let _ = landing_action(
                    &mut columns[0],
                    "Webcam recorder",
                    "Create an animated GIF from a camera.",
                    false,
                );
                let _ = landing_action(
                    &mut columns[1],
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

    fn show_screen_recorder(&mut self, ui: &mut egui::Ui) {
        if self.region_picker.is_some() {
            self.show_region_picker(ui);
            return;
        }
        ui.heading("X11 screen recorder");
        ui.label("Capture and GIF encoding run on a background worker.");
        ui.add_space(12.0);
        self.show_recording_settings(ui);

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.job.is_none(), egui::Button::new("Record GIF"))
                .clicked()
                && let Err(error) = self.start_recording()
            {
                self.notice = Some(error);
            }
            if ui
                .add_enabled(self.job.is_some(), egui::Button::new("Cancel"))
                .clicked()
                && let Some(job) = &self.job
            {
                job.cancellation.cancel();
                self.notice = Some("Cancelling recording…".into());
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
                ui.label("Duration (ms)");
                ui.add(egui::DragValue::new(&mut self.settings.duration_ms).range(1..=60_000));
                ui.end_row();
                ui.label("Frames per second");
                ui.add(egui::DragValue::new(&mut self.settings.fps).range(1..=60));
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

    fn start_recording(&mut self) -> Result<(), String> {
        validate_settings(&self.settings)?;
        let selected = self
            .sources
            .get(self.selected_source)
            .ok_or_else(|| "No X11 capture source is selected.".to_owned())?;
        let settings = self.settings.clone();
        let source_id = selected.id().clone();
        let source_kind = selected.kind();
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();

        std::thread::Builder::new()
            .name("gfs-x11-record".into())
            .spawn(move || {
                let progress_sender = sender.clone();
                let mut progress = move |snapshot| {
                    let _ = progress_sender.send(JobMessage::Progress(snapshot));
                };
                let result = run_x11_recording(
                    &settings,
                    source_id,
                    source_kind,
                    &worker_cancellation,
                    &mut progress,
                )
                .map_err(|error| error.to_string());
                let _ = sender.send(JobMessage::Finished(result));
            })
            .map_err(|error| format!("could not start recording worker: {error}"))?;

        self.notice = Some("Recording started…".into());
        self.progress = None;
        self.job = Some(RecordingJob {
            receiver,
            cancellation,
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
                JobMessage::Finished(Ok(report)) => {
                    self.notice = Some(format!(
                        "Saved {} GIF frames ({} bytes) to {}",
                        report.encoding.encoded_frames,
                        report.bytes_written,
                        report.output_path.display()
                    ));
                    self.job = None;
                }
                JobMessage::Finished(Err(error)) => {
                    self.notice = Some(format!("Recording failed: {error}"));
                    self.job = None;
                }
            }
        }
    }
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

fn validate_settings(settings: &RecordingSettings) -> Result<(), String> {
    if settings.duration_ms == 0 || settings.duration_ms > 60_000 {
        return Err("Duration must be between 1 and 60000 ms.".into());
    }
    if !(1..=60).contains(&settings.fps) {
        return Err("FPS must be between 1 and 60.".into());
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
    Ok(())
}

fn run_x11_recording(
    settings: &RecordingSettings,
    source_id: CaptureSourceId,
    source_kind: CaptureSourceKind,
    cancellation: &CancellationFlag,
    progress: &mut dyn gif_from_screen_workflow::WorkflowProgressSink,
) -> Result<RecordToGifReport, Box<dyn std::error::Error + Send + Sync>> {
    let backend = X11CaptureBackend::connect(None)?;
    let target = if settings.region_enabled {
        CaptureTarget::Region {
            source: source_id,
            region: PhysicalRect::new(
                settings.region_x,
                settings.region_y,
                settings.region_width,
                settings.region_height,
            )?,
        }
    } else {
        match source_kind {
            CaptureSourceKind::Monitor => CaptureTarget::Monitor(source_id),
            CaptureSourceKind::Window => CaptureTarget::Window(source_id),
            _ => return Err("unsupported future X11 capture source kind".into()),
        }
    };
    let mut request = CaptureRequest::new(target, CaptureCadence::fixed_fps(settings.fps)?);
    request.cursor = CursorCaptureMode::Hidden;
    let options = RecordToGifOptions {
        collection: CollectOptions {
            limit: CollectionLimit::Duration(Duration::from_millis(settings.duration_ms)),
            tail_frame_duration: Duration::from_micros(1_000_000 / u64::from(settings.fps)),
            ..CollectOptions::default()
        },
        encoding: EncodeOptions::default(),
    };
    record_to_gif(
        &backend,
        request,
        settings.output.trim(),
        &options,
        cancellation,
        progress,
    )
    .map_err(Into::into)
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
        ui.set_min_width(280.0);
        clicked = ui.add_enabled(enabled, egui::Button::new(title)).clicked();
        ui.label(description);
        if !enabled {
            ui.weak("Planned");
        }
    });
    clicked
}

fn main() -> eframe::Result {
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
        Box::new(|_creation_context| Ok(Box::<GifFromScreenApp>::default())),
    )
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::{
        RecordingSettings, fit_dimensions, map_preview_selection, resize_nearest_rgba,
        validate_settings,
    };

    #[test]
    fn validates_recording_bounds_and_extension() {
        let mut settings = RecordingSettings {
            output: "/tmp/gfs-ui-validation.gif".into(),
            ..RecordingSettings::default()
        };
        assert!(validate_settings(&settings).is_ok());
        settings.fps = 0;
        assert!(validate_settings(&settings).is_err());
        settings.fps = 10;
        settings.output = "capture.mp4".into();
        assert!(validate_settings(&settings).is_err());
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
}
