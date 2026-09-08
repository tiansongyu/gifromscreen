use std::fs;

use eframe::egui;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{
    DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId, Rgba, UnixTimeMs,
};

use super::{InputBoundary, invalidate, update};
use crate::{
    editor_ui::{DrawingDraftPhase, DrawingOverlayDraft, MAX_DRAWING_DRAFT_POINTS},
    editor_workspace::EditorWorkspace,
};

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent renderer/input conditions in the real-egui regression matrix"
)]
struct Scene {
    _directory: tempfile::TempDir,
    workspace: EditorWorkspace,
    context: egui::Context,
    draft: DrawingOverlayDraft,
    boundary: InputBoundary,
    rect: egui::Rect,
    clip: Option<egui::Rect>,
    rendered: [u32; 2],
    widget: u64,
    ppp: f32,
    enabled: bool,
    visible: bool,
    popup: bool,
    overlap: bool,
    duplicate_update: bool,
    discard_pass: bool,
    transform: egui::emath::TSTransform,
}

impl Scene {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut project = create_blank_animation_project(
            directory.path().join("drawing.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(801),
                frame_id: FrameId::from_u128(1),
                app_version: "drawing-input-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(200, 120).unwrap(),
                background: Rgba::TRANSPARENT,
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 200 * 120 * 4,
            },
        )
        .unwrap();
        let second = FrameClip {
            id: FrameId::from_u128(2),
            ..project.manifest().timeline.frames[0].clone()
        };
        project
            .commit(EditCommand::InsertFrames {
                index: 1,
                frames: vec![second],
            })
            .unwrap();
        let mut workspace = EditorWorkspace::from_active(project, 10).unwrap();
        workspace.select_first().unwrap();
        let mut draft = DrawingOverlayDraft::default();
        draft.name = "Keep {name} / 保留".to_owned();
        draft.width = 7;
        draft.z_index = 13;
        draft.begin_for_selection(&workspace).unwrap();
        Self {
            _directory: directory,
            workspace,
            context: egui::Context::default(),
            draft,
            boundary: InputBoundary::default(),
            rect: egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(200.0, 120.0)),
            clip: None,
            rendered: [200, 120],
            widget: 1,
            ppp: 1.0,
            enabled: true,
            visible: true,
            popup: false,
            overlap: false,
            duplicate_update: false,
            discard_pass: false,
            transform: egui::emath::TSTransform::IDENTITY,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>, focused: bool) -> bool {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            events,
            focused,
            ..egui::RawInput::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(self.ppp);
        self.boundary.filter(
            &self.context,
            &mut input,
            self.enabled && self.visible,
            &mut self.draft,
        );
        if self.duplicate_update {
            self.boundary.filter(
                &self.context,
                &mut input,
                self.enabled && self.visible,
                &mut self.draft,
            );
        }
        let mut other_clicked = false;
        let _ = self.context.run(input, |context| {
            if !self.visible {
                invalidate(context, &mut self.draft);
                return;
            }
            egui::CentralPanel::default().show(context, |ui| {
                context.set_transform_layer(ui.layer_id(), self.transform);
                if let Some(clip) = self.clip {
                    ui.set_clip_rect(clip);
                }
                ui.painter()
                    .rect_filled(self.rect, 0.0, egui::Color32::DARK_GRAY);
                let response = ui.interact(
                    self.rect,
                    egui::Id::new(("ordinary-drawing", self.widget)),
                    egui::Sense::click_and_drag(),
                );
                update(ui, &response, self.rendered, self.enabled, &mut self.draft);
                if self.duplicate_update {
                    update(ui, &response, self.rendered, self.enabled, &mut self.draft);
                }
                if self.overlap {
                    other_clicked = ui
                        .put(self.rect.shrink(10.0), egui::Button::new("Other widget"))
                        .clicked();
                }
            });
            if self.popup {
                egui::Window::new("Blocking popup")
                    .id(egui::Id::new("drawing-popup"))
                    .fixed_rect(self.rect)
                    .show(context, |ui| {
                        ui.label("Do not draw here");
                    });
            }
            if self.discard_pass && context.current_pass_index() == 0 {
                context.request_discard("exercise same-frame drawing event replay");
            }
        });
        other_clicked
    }

    fn warm(&mut self) {
        self.frame(Vec::new(), true);
        self.frame(Vec::new(), true);
    }

    fn press(&mut self) -> egui::Pos2 {
        let point = self.transform * (self.rect.min + egui::vec2(50.0, 50.0));
        self.frame(button(point, true), true);
        assert!(self.draft.preview_gesture.active.is_some());
        assert_eq!(self.draft.phase, DrawingDraftPhase::Capturing);
        point
    }

    fn fresh_stroke(&mut self) {
        self.warm();
        let point = self.press();
        self.frame(button(point + egui::vec2(30.0, 20.0), false), true);
        assert_eq!(self.draft.phase, DrawingDraftPhase::Ready);
        assert!(!self.draft.points.is_empty());
    }
}

fn button(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn xy(draft: &DrawingOverlayDraft) -> Vec<(u32, u32)> {
    draft
        .points
        .iter()
        .map(|point| (point.point.x.get(), point.point.y.get()))
        .collect()
}

#[test]
fn real_primary_down_is_recorded_before_threshold_and_release_uses_its_event_position() {
    let mut scene = Scene::new();
    scene.warm();
    let point = scene.press();
    assert_eq!(xy(&scene.draft), [(50, 50)]);
    let mut events = button(point + egui::vec2(20.0, 10.0), false);
    events.push(egui::Event::PointerMoved(point + egui::vec2(90.0, 40.0)));
    events.push(egui::Event::PointerGone);
    scene.frame(events, true);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
    assert_eq!(xy(&scene.draft), [(50, 50), (70, 60)]);
}

#[test]
fn same_frame_down_up_creates_one_dot_and_repeated_passes_cannot_replay_it() {
    for repeated_pass in [false, true] {
        let mut scene = Scene::new();
        scene.duplicate_update = true;
        scene.warm();
        scene.discard_pass = repeated_pass;
        let point = scene.rect.center();
        let mut events = button(point, true);
        events.extend(button(point, false));
        scene.frame(events, true);
        scene.frame(Vec::new(), true); // Raw hook preserves the tail in the next genuine pass.
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
        assert_eq!(xy(&scene.draft), [(100, 60)]);
    }
}

#[test]
fn down_and_held_motion_are_consumed_once_across_duplicate_calls_and_discard_passes() {
    let mut scene = Scene::new();
    scene.warm();
    scene.duplicate_update = true;
    scene.discard_pass = true;
    let point = scene.press();
    assert_eq!(xy(&scene.draft), [(50, 50)]);
    scene.frame(
        vec![egui::Event::PointerMoved(point + egui::vec2(20.0, 10.0))],
        true,
    );
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
    assert_eq!(xy(&scene.draft), [(50, 50), (70, 60)]);
    scene.frame(button(point + egui::vec2(20.0, 10.0), false), true);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
    assert_eq!(xy(&scene.draft), [(50, 50), (70, 60)]);
}

#[test]
fn active_mapping_origin_extent_size_scale_widget_and_clip_changes_abort_even_on_release() {
    for change in 0..7 {
        let mut scene = Scene::new();
        scene.warm();
        let point = scene.press();
        match change {
            0 => scene.rect = scene.rect.translate(egui::vec2(0.0, 30.0)),
            1 => scene.rect.max.x += 40.0,
            2 => scene.rendered = [400, 240],
            3 => scene.ppp = 2.0,
            4 => scene.context.set_zoom_factor(1.5),
            5 => scene.widget = 2,
            _ => scene.clip = Some(scene.rect.shrink(2.0)),
        }
        scene.frame(button(point + egui::vec2(10.0, 10.0), false), true);
        assert_eq!(
            scene.draft.phase,
            DrawingDraftPhase::Capturing,
            "change {change}"
        );
        assert!(scene.draft.points.is_empty(), "change {change}");
        assert!(scene.draft.preview_gesture.active.is_none());
        scene.fresh_stroke();
    }
}

#[test]
fn changed_mapping_before_first_down_rejects_click_against_the_old_displayed_image() {
    let mut scene = Scene::new();
    scene.warm();
    scene.rect = scene.rect.translate(egui::vec2(0.0, 20.0));
    let point = egui::pos2(100.0, 100.0);
    scene.frame(button(point, true), true);
    assert!(scene.draft.points.is_empty());
    assert!(scene.draft.preview_gesture.blocked);
    scene.frame(
        vec![egui::Event::PointerMoved(point + egui::vec2(30.0, 20.0))],
        true,
    );
    assert!(scene.draft.points.is_empty());
    scene.frame(button(point, false), true);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
    scene.fresh_stroke();
}

#[test]
fn pointer_loss_focus_loss_disabled_hidden_and_invalid_geometry_never_resume_the_same_hold() {
    for loss in 0..6 {
        let mut scene = Scene::new();
        scene.warm();
        let point = scene.press();
        let original = scene.rect;
        match loss {
            0 => {
                scene.frame(vec![egui::Event::PointerGone], true);
            }
            1 => {
                scene.frame(vec![egui::Event::WindowFocused(false)], false);
            }
            2 => {
                scene.enabled = false;
                scene.frame(Vec::new(), true);
            }
            3 => {
                scene.visible = false;
                scene.frame(Vec::new(), true);
            }
            4 => {
                scene.rendered = [0, 120];
                scene.frame(Vec::new(), true);
            }
            _ => {
                scene.clip = Some(egui::Rect::NOTHING);
                scene.frame(Vec::new(), true);
            }
        }
        assert_eq!(
            scene.draft.phase,
            DrawingDraftPhase::Capturing,
            "loss {loss}"
        );
        assert!(scene.draft.points.is_empty());
        scene.enabled = true;
        scene.visible = true;
        scene.rect = original;
        scene.rendered = [200, 120];
        scene.clip = None;
        scene.frame(
            vec![egui::Event::PointerMoved(point + egui::vec2(30.0, 10.0))],
            true,
        );
        assert!(scene.draft.points.is_empty());
        scene.frame(button(point, false), true);
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
        scene.fresh_stroke();
    }
}

#[test]
fn hidden_release_is_observed_and_fresh_primary_sequence_can_start_on_return() {
    let mut scene = Scene::new();
    scene.warm();
    let point = scene.press();
    scene.visible = false;
    scene.frame(Vec::new(), true);
    scene.frame(button(point, false), true);
    assert!(!scene.draft.preview_gesture.blocked);
    scene.visible = true;
    scene.fresh_stroke();
}

#[test]
fn popup_or_later_same_layer_widget_owns_its_press_instead_of_the_drawing() {
    for popup in [false, true] {
        let mut scene = Scene::new();
        scene.popup = popup;
        scene.overlap = !popup;
        scene.warm();
        let point = scene.rect.center();
        scene.frame(button(point, true), true);
        let clicked = scene.frame(button(point, false), true);
        assert!(scene.draft.points.is_empty(), "popup={popup}");
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
        if !popup {
            assert!(clicked, "the real overlaid button should receive its click");
        }
        scene.popup = false;
        scene.overlap = false;
        scene.fresh_stroke();
    }
}

#[test]
fn a_press_on_another_widget_cannot_be_reassigned_to_the_image_after_same_batch_motion() {
    for same_batch_release in [false, true] {
        let mut scene = Scene::new();
        scene.overlap = true;
        scene.warm();
        let start = scene.rect.center();
        let outside_button = scene.rect.min + egui::vec2(5.0, 50.0);
        let mut events = button(start, true);
        events.push(egui::Event::PointerMoved(outside_button));
        if same_batch_release {
            events.extend(button(outside_button, false));
        }
        scene.frame(events, true);
        assert!(
            scene.draft.points.is_empty(),
            "same_batch_release={same_batch_release}"
        );
        scene.frame(
            if same_batch_release {
                Vec::new()
            } else {
                button(outside_button, false)
            },
            true,
        );
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
        assert!(scene.draft.points.is_empty());
        scene.overlap = false;
        scene.fresh_stroke();
    }
}

#[test]
fn explicit_rearm_during_an_old_hold_does_not_restart_until_a_new_primary_press() {
    let mut scene = Scene::new();
    scene.warm();
    let point = scene.press();
    scene.draft.cancel();
    scene.draft.begin_for_selection(&scene.workspace).unwrap();
    scene.frame(
        vec![egui::Event::PointerMoved(point + egui::vec2(20.0, 10.0))],
        true,
    );
    assert!(scene.draft.points.is_empty());
    scene.frame(button(point, false), true);
    assert!(scene.draft.points.is_empty());
    scene.fresh_stroke();
}

#[test]
fn entering_with_an_outside_hold_or_other_mouse_button_never_starts_a_stroke() {
    let mut scene = Scene::new();
    scene.warm();
    let outside = egui::pos2(450.0, 250.0);
    scene.frame(button(outside, true), true);
    scene.frame(vec![egui::Event::PointerMoved(scene.rect.center())], true);
    scene.frame(button(scene.rect.center(), false), true);
    assert!(scene.draft.points.is_empty());
    for other in [egui::PointerButton::Middle, egui::PointerButton::Secondary] {
        for pressed in [true, false] {
            let pos = scene.rect.center();
            scene.frame(
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: other,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                true,
            );
        }
    }
    assert!(scene.draft.points.is_empty());
    scene.fresh_stroke();
}

#[test]
fn ready_draft_and_project_selection_style_and_disk_are_unchanged_by_invalidations() {
    let mut scene = Scene::new();
    let before = scene.workspace.manifest().clone();
    let selection = scene.workspace.selection().clone();
    let root = scene.workspace.project_root().to_owned();
    let manifest = fs::read(root.join("manifest.json")).unwrap();
    let journal = fs::read(root.join("journal.ndjson")).unwrap();
    scene.fresh_stroke();
    let ready = format!("{:?}", scene.draft);
    scene.rect = scene.rect.translate(egui::vec2(25.0, 10.0));
    scene.enabled = false;
    scene.frame(vec![egui::Event::PointerGone], false);
    scene.visible = false;
    scene.frame(Vec::new(), false);
    assert_eq!(format!("{:?}", scene.draft), ready);
    assert_eq!(scene.workspace.manifest(), &before);
    assert_eq!(scene.workspace.selection(), &selection);
    assert_eq!(fs::read(root.join("manifest.json")).unwrap(), manifest);
    assert_eq!(fs::read(root.join("journal.ndjson")).unwrap(), journal);
}

#[test]
fn aborted_stroke_keeps_its_explicit_target_anchor_until_selection_reconciliation() {
    let mut scene = Scene::new();
    scene.warm();
    scene.press();
    scene.enabled = false;
    scene.frame(Vec::new(), true);
    assert_eq!(scene.draft.name, "Keep {name} / 保留");
    assert_eq!(scene.draft.width, 7);
    assert_eq!(scene.draft.z_index, 13);
    scene.draft.reconcile(&scene.workspace);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
    scene.workspace.select_only(FrameId::from_u128(2)).unwrap();
    scene.draft.reconcile(&scene.workspace);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Idle);
}

#[test]
fn point_limit_keeps_a_usable_ready_draft_and_never_grows_on_late_held_motion() {
    let mut scene = Scene::new();
    scene.rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(8192.0, 120.0));
    scene.rendered = [8192, 120];
    scene.warm();
    let point = scene.rect.min + egui::vec2(0.25, 50.0);
    scene.frame(button(point, true), true);
    let events = (1..=MAX_DRAWING_DRAFT_POINTS)
        .map(|index| {
            egui::Event::PointerMoved(
                point + egui::vec2(f32::from(u16::try_from(index).unwrap()), 0.0),
            )
        })
        .collect();
    scene.frame(events, true);
    assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
    assert_eq!(scene.draft.points.len(), MAX_DRAWING_DRAFT_POINTS);
    assert!(scene.draft.limit_reached);
    let ready = format!("{:?}", scene.draft);
    scene.frame(
        vec![egui::Event::PointerMoved(point), egui::Event::PointerGone],
        true,
    );
    assert_eq!(format!("{:?}", scene.draft), ready);
}

#[test]
fn unrelated_and_mixed_raw_event_overflow_aborts_instead_of_scanning_past_the_budget() {
    for include_up in [false, true] {
        let mut scene = Scene::new();
        scene.warm();
        let point = scene.press();
        let mut events = vec![egui::Event::Copy; super::MAX_INPUT_EVENTS + 1];
        if include_up {
            events[0] = egui::Event::PointerMoved(point + egui::vec2(20.0, 10.0));
            events.extend(button(point, false));
        }
        scene.frame(events, true);
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
        assert!(scene.draft.points.is_empty());
        assert!(scene.draft.preview_gesture.active.is_none());
        scene.frame(button(point, false), true);
        scene.fresh_stroke();
    }
}

#[test]
fn fast_down_move_and_release_batches_are_delivered_without_a_manual_hold() {
    for release_in_initial_batch in [false, true] {
        let mut scene = Scene::new();
        scene.duplicate_update = true;
        scene.discard_pass = true;
        scene.warm();
        let start = scene.rect.min + egui::vec2(20.0, 30.0);
        let middle = start + egui::vec2(40.0, 10.0);
        let end = start + egui::vec2(70.0, 20.0);
        let mut events = button(start, true);
        events.push(egui::Event::PointerMoved(middle));
        events.push(egui::Event::PointerMoved(end));
        if release_in_initial_batch {
            events.extend(button(end, false));
            events.push(egui::Event::PointerMoved(egui::pos2(400.0, 350.0)));
        }
        scene.frame(events, true);
        assert_eq!(xy(&scene.draft), [(20, 30)]);
        assert!(scene.draft.preview_gesture.active.is_some());
        scene.frame(
            if release_in_initial_batch {
                Vec::new()
            } else {
                button(end, false)
            },
            true,
        );
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
        assert_eq!(xy(&scene.draft), [(20, 30), (60, 40), (90, 50)]);
        // Explicitly arming a second stroke cannot replay the first one's tail.
        scene.draft.begin();
        scene.warm();
        let mut second = button(start, true);
        second.extend(button(middle, false));
        scene.frame(second, true);
        scene.frame(Vec::new(), true);
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Ready);
        assert_eq!(xy(&scene.draft), [(20, 30), (60, 40)]);
    }
}

#[test]
fn layer_transform_is_applied_once_and_changes_abort_the_locked_mapping() {
    for change_before_down in [false, true] {
        let mut scene = Scene::new();
        scene.transform = egui::emath::TSTransform::new(egui::vec2(12.0, 18.0), 1.5);
        scene.warm();
        let initial = scene.transform * (scene.rect.min + egui::vec2(50.0, 50.0));
        if !change_before_down {
            scene.press();
            assert_eq!(xy(&scene.draft), [(50, 50)]);
        }
        scene.transform = egui::emath::TSTransform::new(egui::vec2(22.0, 28.0), 1.25);
        scene.frame(button(initial, change_before_down), true);
        assert_eq!(scene.draft.phase, DrawingDraftPhase::Capturing);
        assert!(scene.draft.points.is_empty());
        assert!(scene.draft.preview_gesture.active.is_none());
        scene.frame(button(initial, false), true);
        scene.fresh_stroke();
        assert_eq!(xy(&scene.draft)[0], (50, 50));
    }
}
