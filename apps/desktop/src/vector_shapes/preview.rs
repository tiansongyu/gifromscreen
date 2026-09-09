use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use gif_from_screen_render::{
    CancellationToken, RenderLimits, RgbaSurface, render_vector_shapes_preview,
};

use super::{draft::Draft, error};
use crate::{background_task::BackgroundTask, ui_notice::Notice};

const PREVIEW_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Key {
    generation: u64,
    canvas: [u32; 2],
    output: [u32; 2],
}

struct Cancel<'a>(&'a AtomicBool);
impl CancellationToken for Cancel<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub(super) struct RasterPreview {
    task: BackgroundTask<RgbaSurface, ()>,
    running: Option<Key>,
    desired: Option<Key>,
    ready: Option<Key>,
    failed: Option<Key>,
    texture: Option<egui::TextureHandle>,
    pub(super) failure: Option<Notice>,
}

impl RasterPreview {
    pub(super) fn is_running(&self) -> bool {
        self.task.is_running()
    }
    pub(super) fn invalidate(&mut self) {
        self.task.cancel();
        self.desired = None;
        self.ready = None;
        self.failed = None;
        self.texture = None;
        self.failure = None;
    }
    pub(super) fn retry(&mut self) {
        self.invalidate();
    }

    pub(super) fn poll(&mut self, context: &egui::Context) {
        if let Some(result) = self.task.poll() {
            let key = self.running.take();
            if key.is_some() && key == self.desired {
                match result {
                    Ok(surface) => {
                        let size = [surface.width() as usize, surface.height() as usize];
                        let image =
                            egui::ColorImage::from_rgba_unmultiplied(size, surface.pixels());
                        self.texture = Some(context.load_texture(
                            "vector-shape-draft",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                        self.ready = key;
                        self.failed = None;
                        self.failure = None;
                    }
                    Err(reason) => {
                        self.failed = key;
                        self.ready = None;
                        self.texture = None;
                        self.failure = Some(error(reason));
                    }
                }
            }
            context.request_repaint();
        }
        if self.is_running() {
            context.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    pub(super) fn request(&mut self, context: &egui::Context, draft: &Draft, output: [u32; 2]) {
        let Some(canvas) = draft.canvas() else {
            self.invalidate();
            return;
        };
        if draft.stale {
            self.invalidate();
            return;
        }
        let key = Key {
            generation: draft.generation,
            canvas,
            output,
        };
        if self.desired != Some(key) {
            self.desired = Some(key);
            self.ready = None;
            self.failed = None;
            self.texture = None;
            self.failure = None;
            if self.running != Some(key) {
                self.task.cancel();
            }
        }
        self.poll(context);
        if self.task.is_running() || self.ready == Some(key) || self.failed == Some(key) {
            return;
        }
        let shapes = draft
            .objects
            .iter()
            .map(|object| object.shape)
            .collect::<Vec<_>>();
        match self.task.start("vector-shape-preview", move |task| {
            render_vector_shapes_preview(
                &shapes,
                canvas,
                output,
                RenderLimits {
                    max_surface_bytes: PREVIEW_BYTES,
                },
                &Cancel(task.cancellation()),
            )
            .map_err(|error| error.to_string())
        }) {
            Ok(()) => {
                self.running = Some(key);
                context.request_repaint();
            }
            Err(reason) => {
                self.failed = Some(key);
                self.failure = Some(error(reason));
            }
        }
    }

    pub(super) fn current(&self, generation: u64) -> bool {
        self.ready
            .is_some_and(|key| key.generation == generation && Some(key) == self.desired)
    }
    pub(super) fn paint(&self, painter: &egui::Painter, rect: egui::Rect, generation: u64) {
        if self.current(generation)
            && let Some(texture) = &self.texture
        {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}
