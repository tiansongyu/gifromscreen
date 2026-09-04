#![forbid(unsafe_code)]

//! Desktop entry point for the Linux-first `GifFromScreen` application.

use eframe::egui;

const APP_NAME: &str = "GifFromScreen";

#[derive(Default)]
struct GifFromScreenApp {
    notice: Option<&'static str>,
}

impl eframe::App for GifFromScreenApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("app_header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading(APP_NAME);
                ui.separator();
                ui.label("Linux-first development preview");
            });
        });

        egui::CentralPanel::default().show(context, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("Create an animated GIF");
                ui.label("Capture, edit frame by frame, and export locally.");
                ui.add_space(28.0);

                ui.columns(2, |columns| {
                    landing_action(
                        &mut columns[0],
                        "Screen recorder",
                        "Record a monitor, window, or selected region.",
                        &mut self.notice,
                    );
                    landing_action(
                        &mut columns[1],
                        "Open or import",
                        "Open a project, GIF, image sequence, or video.",
                        &mut self.notice,
                    );
                });

                ui.add_space(12.0);

                ui.columns(2, |columns| {
                    landing_action(
                        &mut columns[0],
                        "Webcam recorder",
                        "Create an animated GIF from a camera.",
                        &mut self.notice,
                    );
                    landing_action(
                        &mut columns[1],
                        "Drawing board",
                        "Record drawing strokes as an animation.",
                        &mut self.notice,
                    );
                });

                if let Some(notice) = self.notice {
                    ui.add_space(24.0);
                    ui.label(notice);
                }
            });
        });
    }
}

fn landing_action(
    ui: &mut egui::Ui,
    title: &'static str,
    description: &'static str,
    notice: &mut Option<&'static str>,
) {
    ui.group(|ui| {
        ui.set_min_height(112.0);
        ui.set_min_width(280.0);
        if ui.button(title).clicked() {
            *notice =
                Some("This entry point is being connected by the current Linux vertical slice.");
        }
        ui.label(description);
    });
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([760.0, 520.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|_creation_context| Ok(Box::<GifFromScreenApp>::default())),
    )
}
