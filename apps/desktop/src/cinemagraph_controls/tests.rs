//! Headless interaction tests exercise the real editor's outer scrolling host.

use super::*;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{DurationUs, PhysicalSize, ProjectId, Rgba, UnixTimeMs};
use gif_from_screen_render::{InkPoint, InkSample};
use std::sync::atomic::Ordering;

fn fixture() -> (tempfile::TempDir, EditorWorkspace, MotionTools) {
    let directory = tempfile::tempdir().unwrap();
    let project = create_blank_animation_project(
        directory.path(),
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(815),
            frame_id: FrameId::from_u128(12),
            app_version: "cinemagraph-controls-test".into(),
            created_at: UnixTimeMs::new(0),
            canvas: PhysicalSize::new(100, 100).unwrap(),
            background: Rgba {
                red: 20,
                green: 30,
                blue: 40,
                alpha: 255,
            },
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 64 * 1024,
        },
    )
    .unwrap();
    let mut workspace = EditorWorkspace::from_active(project, 16).unwrap();
    workspace.select_first().unwrap();
    let mut tool = MotionTools {
        mode: Mode::Cinemagraph,
        ..MotionTools::default()
    };
    tool.cine.begin(&workspace).unwrap();
    let sample = sample();
    tool.cine.pointer_down(sample).unwrap();
    tool.cine.pointer_up(sample).unwrap();
    (directory, workspace, tool)
}

fn sample() -> InkSample {
    InkSample {
        position: InkPoint {
            x: 25.25,
            y: 30.125,
        },
        pressure: 0.5,
    }
}

struct View {
    context: egui::Context,
    size: egui::Vec2,
    full_motion: bool,
    enabled: bool,
}

impl View {
    fn new(width: f32, height: f32, font_scale: f32) -> Self {
        let context = egui::Context::default();
        context.style_mut(|style| {
            style.animation_time = 0.0;
            for font in style.text_styles.values_mut() {
                font.size *= font_scale;
            }
        });
        Self {
            context,
            size: egui::vec2(width, height),
            full_motion: false,
            enabled: true,
        }
    }

    fn draw(
        &self,
        tool: &mut MotionTools,
        workspace: &EditorWorkspace,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        self.context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                events,
                focused: true,
                ..egui::RawInput::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    crate::show_editor_scroll_area(ui, |ui| {
                        // The real editor has the canvas/inspector above Motion tools.
                        if !self.full_motion {
                            ui.add_space(220.0);
                        }
                        if self.full_motion {
                            tool.show(ui, workspace);
                        } else {
                            ui.add_enabled_ui(self.enabled && !tool.is_running(), |ui| {
                                tool.show_cinemagraph_controls(ui, workspace);
                            });
                            if tool.is_running() {
                                tool.show_running(ui);
                            }
                        }
                    });
                });
            },
        )
    }

    fn click(&self, tool: &mut MotionTools, workspace: &EditorWorkspace, position: egui::Pos2) {
        for pressed in [true, false] {
            self.draw(
                tool,
                workspace,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    fn seek(&self, tool: &mut MotionTools, workspace: &EditorWorkspace, label: &str) -> egui::Rect {
        for _ in 0..160 {
            let output = self.draw(tool, workspace, Vec::new());
            if let Some(rect) = visible_text(&output, label) {
                return rect;
            }
            self.draw(
                tool,
                workspace,
                vec![
                    egui::Event::PointerMoved(egui::pos2(self.size.x - 14.0, self.size.y * 0.5)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -24.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        panic!("{label} is unreachable at {:?}", self.size);
    }
}

fn visible_text(output: &egui::FullOutput, label: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|clipped| {
        if let egui::Shape::Text(text) = &clipped.shape
            && text.galley.text() == label
        {
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            if clipped.clip_rect.contains_rect(rect) && rect.is_positive() {
                Some(rect)
            } else {
                None
            }
        } else {
            None
        }
    })
}

#[test]
fn apply_and_close_remain_reachable_with_small_windows_large_fonts_and_all_tools() {
    for (width, height, scale) in [
        (320.0, 240.0, 1.0),
        (320.0, 240.0, 1.8),
        (240.0, 240.0, 2.0),
    ] {
        for pen_tool in [
            CinemagraphTool::Pen,
            CinemagraphTool::PointEraser,
            CinemagraphTool::StrokeEraser,
            CinemagraphTool::Select,
        ] {
            for label in ["Apply Cinemagraph", "Close draft"] {
                let (_root, workspace, mut tool) = fixture();
                let before = workspace.manifest().clone();
                tool.cine.tool = pen_tool;
                let view = View::new(width, height, scale);
                let rect = view.seek(&mut tool, &workspace, label);
                view.click(&mut tool, &workspace, rect.center());
                if label == "Apply Cinemagraph" {
                    assert!(tool.pending.is_some(), "{pen_tool:?}");
                } else {
                    assert!(!tool.cine.is_active(), "{pen_tool:?}");
                }
                assert_eq!(workspace.manifest(), &before);
            }
        }
    }
}

#[test]
fn changing_tool_rolls_back_an_active_gesture_and_preserves_completed_ink() {
    for label in ["Erase part", "Erase stroke", "Select"] {
        let (_root, workspace, mut tool) = fixture();
        let completed = tool.cine.strokes().to_vec();
        tool.cine.pointer_down(sample()).unwrap();
        let view = View::new(800.0, 1000.0, 1.0);
        let button = view.seek(&mut tool, &workspace, label);
        view.click(&mut tool, &workspace, button.center());
        assert!(!tool.cine.gesture_active());
        assert_eq!(tool.cine.strokes(), completed);
    }
}

#[test]
fn changing_mode_rolls_back_in_the_same_controls_update() {
    let (_root, workspace, mut tool) = fixture();
    let completed = tool.cine.strokes().to_vec();
    let mut view = View::new(800.0, 1000.0, 1.0);
    view.full_motion = true;
    let header = view.seek(&mut tool, &workspace, "Motion tools");
    view.click(&mut tool, &workspace, header.center());
    let button = view.seek(&mut tool, &workspace, "Rectangular freeze");
    tool.cine.pointer_down(sample()).unwrap();
    view.click(&mut tool, &workspace, button.center());
    assert!(tool.mode == Mode::RectangularFreeze);
    assert!(
        !tool.cine.gesture_active(),
        "mode changes must not leave a live hidden gesture until next frame"
    );
    assert_eq!(tool.cine.strokes(), completed);
}

#[test]
fn active_gesture_disables_apply_but_close_can_cancel_it() {
    let (_root, workspace, mut tool) = fixture();
    tool.cine.pointer_down(sample()).unwrap();
    let view = View::new(800.0, 1000.0, 1.0);
    let apply = view.seek(&mut tool, &workspace, "Apply Cinemagraph");
    view.click(&mut tool, &workspace, apply.center());
    assert!(tool.pending.is_none());
    assert!(tool.cine.gesture_active());
    let close = view.seek(&mut tool, &workspace, "Close draft");
    view.click(&mut tool, &workspace, close.center());
    assert!(!tool.cine.is_active());
    assert!(!tool.cine.gesture_active());
}

#[test]
fn busy_controls_cannot_change_draft_and_cancel_remains_available() {
    let (_root, workspace, mut tool) = fixture();
    tool.queue(&workspace).unwrap();
    let before = tool.cine.strokes().to_vec();
    let view = View::new(800.0, 1200.0, 1.0);
    for label in [
        "Apply Cinemagraph",
        "Clear strokes",
        "Close draft",
        "Erase part",
    ] {
        let button = view.seek(&mut tool, &workspace, label);
        view.click(&mut tool, &workspace, button.center());
        assert!(tool.pending.is_some());
        assert!(tool.cine.is_active());
        assert_eq!(tool.cine.strokes(), before);
    }
    let cancel = view.seek(&mut tool, &workspace, "Cancel motion edit");
    view.click(&mut tool, &workspace, cancel.center());
    assert!(tool.cancel_pending.load(Ordering::Acquire));
}

#[test]
fn stale_draft_cannot_apply_but_close_and_checked_restart_are_available() {
    let (_root, mut workspace, mut tool) = fixture();
    let before = tool.cine.strokes().to_vec();
    tool.cine.pointer_down(sample()).unwrap();
    workspace.clear_selection();
    let view = View::new(320.0, 240.0, 1.8);
    let restart = view.seek(&mut tool, &workspace, "Restart with current targets");
    assert!(tool.cine.is_stale());
    assert!(!tool.cine.gesture_active());
    assert_eq!(tool.cine.strokes(), before);
    view.click(&mut tool, &workspace, restart.center());
    assert!(tool.cine.is_stale());
    assert!(tool.pending.is_none());
    assert!(tool.notice.is_some());
    let close = view.seek(&mut tool, &workspace, "Close draft");
    view.click(&mut tool, &workspace, close.center());
    assert!(!tool.cine.is_active());
}

#[test]
fn empty_draft_and_external_busy_state_do_not_enable_an_edit() {
    let (_root, workspace, mut tool) = fixture();
    let before = workspace.manifest().clone();
    tool.cine.clear().unwrap();
    let mut view = View::new(800.0, 1000.0, 1.0);
    let apply = view.seek(&mut tool, &workspace, "Apply Cinemagraph");
    view.click(&mut tool, &workspace, apply.center());
    assert!(tool.pending.is_none());
    tool.cine.close();
    view.enabled = false;
    let begin = view.seek(&mut tool, &workspace, "Draw motion region");
    view.click(&mut tool, &workspace, begin.center());
    assert!(!tool.cine.is_active());
    assert!(tool.pending.is_none());
    assert_eq!(workspace.manifest(), &before);
}
