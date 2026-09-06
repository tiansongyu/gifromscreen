use super::*;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{
    AnnotationRequest, EdgeWidths, EditingTask, EditingTaskSources, FrameId, PhysicalSize,
    ProjectId, Rgba, UnixTimeMs,
};
use gif_from_screen_project::LockPolicy;
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn workspace(path: &Path) -> EditorWorkspace {
    let project = create_blank_animation_project(
        path,
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(7),
            frame_id: FrameId::from_u128(1),
            app_version: "auto-task-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            canvas: PhysicalSize::new(8, 8).unwrap(),
            background: Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 1024,
        },
    )
    .unwrap();
    let mut workspace = EditorWorkspace::from_active(project, 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

fn preset(actions: Vec<EditingTaskAction>) -> EditingTaskPreset {
    EditingTaskPreset {
        name: "Demo".into(),
        sources: EditingTaskSources::default(),
        tasks: actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| EditingTask {
                name: format!("Task {}", index + 1),
                enabled: true,
                action,
            })
            .collect(),
    }
}

fn delay(ms: u64) -> EditingTaskAction {
    EditingTaskAction::Delay {
        mode: TaskDelay::Override { milliseconds: ms },
    }
}
fn border() -> EditingTaskAction {
    EditingTaskAction::Border {
        widths: EdgeWidths {
            top: 1,
            right: 1,
            bottom: 1,
            left: 1,
        },
        color: Rgba {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        },
    }
}

fn wait_loaded(tool: &mut AutoTasks, slot: &mut Option<EditorWorkspace>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while tool.is_loading() {
        tool.poll(slot);
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
}

fn wait_run(tool: &mut AutoTasks, slot: &mut Option<EditorWorkspace>) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notice = None;
    while tool.is_running() || tool.is_loading() {
        notice = tool.poll(slot).or(notice);
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    notice
}

#[test]
fn chain_order_is_atomic_reversible_and_persisted_with_completion_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project");
    let mut current = workspace(&path);
    let before = current.manifest().clone();
    let chain = preset(vec![
        delay(200),
        EditingTaskAction::Delay {
            mode: TaskDelay::Scale { percent: 50 },
        },
        border(),
    ]);
    let summary = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::ScreenRecording,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(summary.completed.len(), 3);
    assert_eq!(current.manifest().revision, before.revision.next().unwrap());
    assert_eq!(
        current.manifest().timeline.frames[0].duration.get(),
        100_000
    );
    assert_eq!(
        gif_from_screen_editor::frame_effect_count(&current.manifest().timeline.frames[0]),
        1
    );
    assert_eq!(
        current.manifest().task_runs[0].completed_tasks,
        summary.completed
    );
    let after = current.manifest().clone();
    assert!(current.undo().unwrap());
    let mut expected = before;
    expected.revision = current.manifest().revision;
    assert_eq!(current.manifest(), &expected);
    assert!(current.redo().unwrap());
    let mut expected_after = after;
    expected_after.revision = current.manifest().revision;
    assert_eq!(current.manifest(), &expected_after);
    drop(current);
    let reopened = EditorWorkspace::open(&path, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(reopened.manifest(), &expected_after);
}

#[test]
fn new_canvas_effect_tasks_share_interactive_order_and_one_durable_undo() {
    use gif_from_screen_domain::{ImageBorderStyle, ImageShadowStyle, SignedEdgeWidths};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image-effects");
    let mut current = workspace(&path);
    let before = current.manifest().clone();
    let chain = preset(vec![
        EditingTaskAction::ImageShadow {
            style: ImageShadowStyle {
                blur_radius_hundredths: 400,
                depth_hundredths: 200,
                direction_hundredths: 0,
                ..ImageShadowStyle::default()
            },
        },
        EditingTaskAction::ImageBorder {
            style: ImageBorderStyle {
                widths: SignedEdgeWidths {
                    left_milli: -2000,
                    bottom_milli: -1000,
                    ..SignedEdgeWidths::default()
                },
                ..ImageBorderStyle::default()
            },
        },
    ]);
    let summary = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::ScreenRecording,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(summary.completed.len(), 2);
    assert_eq!(
        current.manifest().canvas.size,
        PhysicalSize::new(16, 13).unwrap()
    );
    let frame = &current.manifest().timeline.frames[0];
    assert_eq!(gif_from_screen_editor::frame_effect_count(frame), 2);
    assert_eq!(frame.asset_id, before.timeline.frames[0].asset_id);
    assert_eq!(
        frame.capture_metadata,
        before.timeline.frames[0].capture_metadata
    );
    let pixels = crate::editor_preview::render_frame_surface(
        current.active_project(),
        frame.id,
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(pixels.size(), current.manifest().canvas.size);
    let after = current.manifest().clone();
    assert!(current.undo().unwrap());
    assert_eq!(current.manifest().timeline, before.timeline);
    assert_eq!(current.manifest().canvas, before.canvas);
    assert!(current.redo().unwrap());
    assert_eq!(current.manifest().timeline, after.timeline);
    drop(current);
    let reopened = EditorWorkspace::open(&path, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(
        crate::editor_preview::render_frame_surface(
            reopened.active_project(),
            after.timeline.frames[0].id,
            1024 * 1024
        )
        .unwrap(),
        pixels
    );
}

#[test]
fn saving_new_canvas_tasks_upgrades_old_settings_only_on_explicit_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("editing.json");
    let store = AutoTaskStore::new(path.clone());
    let mut legacy = EditingTaskSettings {
        version: 1,
        enabled: false,
        active_preset: Some("Demo".into()),
        presets: vec![preset(vec![border()])],
    };
    let initial = store.load().unwrap();
    store.save(&initial, legacy.clone()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut tool = AutoTasks::new(path.clone());
    let mut slot = None;
    wait_loaded(&mut tool, &mut slot);
    assert_eq!(tool.draft.version, 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    legacy.presets[0].tasks.push(EditingTask {
        name: "New shadow".into(),
        enabled: true,
        action: EditingTaskAction::ImageShadow {
            style: gif_from_screen_domain::ImageShadowStyle::default(),
        },
    });
    tool.draft = legacy;
    tool.save();
    wait_loaded(&mut tool, &mut slot);
    let saved = store.load().unwrap().config;
    assert_eq!(saved.version, 2);
    assert!(matches!(
        saved.presets[0].tasks[0].action,
        EditingTaskAction::Border { .. }
    ));
    assert!(matches!(
        saved.presets[0].tasks[1].action,
        EditingTaskAction::ImageShadow { .. }
    ));
}

#[test]
fn late_failure_and_cancellation_leave_project_and_undo_history_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    current
        .override_selection_duration(DurationUs::new(300_000).unwrap())
        .unwrap();
    let before = current.manifest().clone();
    let chain = preset(vec![
        delay(100),
        EditingTaskAction::Delay {
            mode: TaskDelay::Adjust { milliseconds: -200 },
        },
    ]);
    let error = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::Manual,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.contains("Task 2"));
    assert_eq!(current.manifest(), &before);
    assert!(current.can_undo());
    let cancellation = AtomicBool::new(false);
    let error = apply_task_chain(
        &mut current,
        &preset(vec![delay(100), border()]),
        EditTaskTrigger::Manual,
        &cancellation,
        |_| cancellation.store(true, Ordering::Release),
    )
    .unwrap_err();
    assert!(error.contains("Cancelled"));
    assert_eq!(current.manifest(), &before);
}

#[test]
fn disabled_tasks_and_non_screen_input_filters_are_reported_without_inventing_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        ..AnnotationRequest::default()
    };
    let mut chain = preset(vec![
        delay(200),
        EditingTaskAction::Annotation { request },
        border(),
    ]);
    chain.tasks[0].enabled = false;
    let result = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::Import,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.completed, ["Task 3"]);
    assert_eq!(result.skipped, ["Task 2"]);
    assert_eq!(
        current.manifest().timeline.frames[0].duration.get(),
        100_000
    );
    let before = current.manifest().clone();
    chain.sources.import = false;
    assert_eq!(
        apply_task_chain(
            &mut current,
            &chain,
            EditTaskTrigger::Import,
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap(),
        TaskSummary::default()
    );
    assert_eq!(current.manifest(), &before);
}

#[test]
fn no_recorded_events_skip_unused_large_boxes_and_allow_later_tasks_to_finish() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let before = current.manifest().clone();
    let mut actions = [
        AnnotationMode::RecordedKeys,
        AnnotationMode::RecordedClicks,
        AnnotationMode::RecordedCursor,
    ]
    .into_iter()
    .map(|mode| EditingTaskAction::Annotation {
        request: AnnotationRequest {
            mode,
            ..AnnotationRequest::default()
        },
    })
    .collect::<Vec<_>>();
    actions.extend([delay(200), border()]);
    let result = apply_task_chain(
        &mut current,
        &preset(actions),
        EditTaskTrigger::ScreenRecording,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.skipped, ["Task 1", "Task 2", "Task 3"]);
    assert_eq!(result.completed, ["Task 4", "Task 5"]);
    assert_eq!(
        current.manifest().timeline.frames[0].duration.get(),
        200_000
    );
    assert_eq!(
        gif_from_screen_editor::frame_effect_count(&current.manifest().timeline.frames[0]),
        1
    );
    assert_eq!(
        current.manifest().task_runs[0].skipped_tasks,
        result.skipped
    );
    assert!(current.undo().unwrap());
    let mut expected = before;
    expected.revision = current.manifest().revision;
    assert_eq!(current.manifest(), &expected);
}

#[test]
fn an_explicit_progress_box_outside_the_canvas_is_not_silently_resized() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let before = current.manifest().clone();
    let chain = preset(vec![
        delay(200),
        EditingTaskAction::Annotation {
            request: AnnotationRequest::default(),
        },
    ]);
    let request_before = chain.clone();
    let error = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::Manual,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.contains("Task 2"));
    assert!(error.contains("canvas"));
    assert_eq!(chain, request_before);
    assert_eq!(current.manifest(), &before);
}

#[test]
fn pending_new_project_waits_for_configuration_and_background_run_keeps_undo() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("tasks.json");
    let store = AutoTaskStore::new(settings_path.clone());
    let previous = store.load().unwrap();
    store
        .save(
            &previous,
            EditingTaskSettings {
                enabled: true,
                active_preset: Some("Demo".to_owned()),
                presets: vec![preset(vec![delay(222), border()])],
                ..EditingTaskSettings::default()
            },
        )
        .unwrap();
    let mut tool = AutoTasks::new(settings_path);
    let mut slot = Some(workspace(&dir.path().join("project")));
    tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::ScreenRecording)
        .unwrap();
    assert!(tool.is_running());
    assert!(
        tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::Import)
            .is_err()
    );
    assert!(
        wait_run(&mut tool, &mut slot)
            .unwrap()
            .contains("2 applied")
    );
    let current = slot.as_mut().unwrap();
    assert_eq!(
        current.manifest().timeline.frames[0].duration.get(),
        222_000
    );
    assert!(current.undo().unwrap());
    assert_eq!(
        current.manifest().timeline.frames[0].duration.get(),
        100_000
    );
    assert!(current.manifest().task_runs.is_empty());
}

#[test]
fn malformed_configuration_does_not_drop_or_silently_edit_new_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    std::fs::write(&path, "broken").unwrap();
    let mut tool = AutoTasks::new(path.clone());
    let mut slot = Some(workspace(&dir.path().join("project")));
    let before = slot.as_ref().unwrap().manifest().clone();
    tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::Import)
        .unwrap();
    assert!(
        wait_run(&mut tool, &mut slot)
            .unwrap()
            .contains("could not load")
    );
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
}

#[test]
fn normal_project_poll_never_reapplies_tasks_and_pending_cancel_preserves_project() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = AutoTasks::new(dir.path().join("tasks.json"));
    let mut slot = Some(workspace(&dir.path().join("project")));
    wait_loaded(&mut tool, &mut slot);
    let before = slot.as_ref().unwrap().manifest().clone();
    assert!(tool.poll(&mut slot).is_none());
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
    tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::Import)
        .unwrap();
    tool.cancel();
    assert!(!tool.is_running());
    tool.poll(&mut slot);
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}

#[test]
fn panicking_worker_preserves_loaned_project_and_prior_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = AutoTasks::new(dir.path().join("tasks.json"));
    let current = workspace(&dir.path().join("project"));
    let before = current.manifest().clone();
    let loan = Arc::new(Mutex::new(Some(current)));
    let worker_loan = Arc::clone(&loan);
    tool.loan = Some(loan);
    tool.job
        .start("auto-task-panic-test", move |_| {
            let _held = worker_loan.lock().unwrap();
            panic!("simulated worker failure");
        })
        .unwrap();
    let mut slot = None;
    assert!(wait_run(&mut tool, &mut slot).unwrap().contains("stopped"));
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}

#[test]
fn all_six_reference_task_classes_share_one_journal_edit_and_frozen_assets() {
    use gif_from_screen_domain::{
        KeyStroke, MouseButton, MouseInputEvent, PhysicalPoint, ProgressOptions, TimeUs,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let mut frame = current.manifest().timeline.frames[0].clone();
    frame.capture_metadata.captured_at = Some(TimeUs::ZERO);
    frame.capture_binding = gif_from_screen_domain::CaptureBinding::Original;
    frame.capture_metadata.key_strokes.push(KeyStroke {
        physical_key: "KeyA".into(),
        display_text: Some("A".into()),
        pressed: true,
        at: TimeUs::ZERO,
        repeat: false,
        modifiers: 0,
    });
    frame.capture_metadata.mouse_events.push(MouseInputEvent {
        at: TimeUs::ZERO,
        button: MouseButton::Left,
        pressed: true,
        position: Some(PhysicalPoint::default()),
    });
    current
        .execute(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        })
        .unwrap();
    let before = current.manifest().clone();
    let annotation = |mode| EditingTaskAction::Annotation {
        request: AnnotationRequest {
            mode,
            size: PhysicalSize::new(8, 8).unwrap(),
            font_size_px: 4,
            click_radius: 2,
            ..AnnotationRequest::default()
        },
    };
    let chain = preset(vec![
        annotation(AnnotationMode::RecordedClicks),
        annotation(AnnotationMode::RecordedKeys),
        delay(200),
        annotation(AnnotationMode::Progress(ProgressOptions {
            format: String::new(),
            ..ProgressOptions::default()
        })),
        border(),
        EditingTaskAction::Shadow {
            offset_x: 1,
            offset_y: 1,
            blur_radius: 0,
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128,
            },
        },
    ]);
    let result = apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::ScreenRecording,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.completed.len(), 6);
    assert_eq!(current.manifest().timeline.overlay_tracks.len(), 3);
    assert_eq!(
        gif_from_screen_editor::frame_effect_count(&current.manifest().timeline.frames[0]),
        2
    );
    assert!(current.manifest().assets.len() > before.assets.len());
    for asset in current.manifest().assets.values() {
        assert_eq!(
            current.active_project().assets().verify(asset.id).unwrap(),
            asset.byte_len
        );
    }
    let preview = crate::editor_preview::render_frame_surface(
        current.active_project(),
        current.manifest().timeline.frames[0].id,
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(preview.size(), current.manifest().canvas.size);
    assert!(current.undo().unwrap());
    let mut expected = before;
    expected.revision = current.manifest().revision;
    assert_eq!(current.manifest(), &expected);
}

#[test]
fn metadata_budget_counts_without_allocating_and_defaults_are_isolated() {
    let mut budget = ByteBudget(3);
    assert!(serde_json::to_writer(&mut budget, &vec![1, 2, 3]).is_err());
    let one = AutoTasks::default();
    let two = AutoTasks::default();
    assert_ne!(
        one.test_directory.as_ref().unwrap().path(),
        two.test_directory.as_ref().unwrap().path()
    );
    assert!(
        AutoTaskStore::new(PathBuf::from("relative/tasks.json"))
            .load()
            .is_err()
    );
}

#[test]
fn failed_settings_save_preserves_external_file_and_requires_reload_before_auto_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let mut tool = AutoTasks::new(path.clone());
    let mut slot = Some(workspace(&dir.path().join("project")));
    wait_loaded(&mut tool, &mut slot);
    tool.draft.enabled = true;
    tool.draft.active_preset = Some("Demo".to_owned());
    tool.draft.presets.push(preset(vec![delay(333)]));
    std::fs::write(&path, "external invalid file").unwrap();
    tool.save();
    wait_loaded(&mut tool, &mut slot);
    assert!(tool.settings_error.is_some());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "external invalid file"
    );
    let before = slot.as_ref().unwrap().manifest().clone();
    tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::ScreenRecording)
        .unwrap();
    assert!(
        wait_run(&mut tool, &mut slot)
            .unwrap()
            .contains("load/save error")
    );
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}

#[test]
fn changing_project_before_a_pending_run_cannot_apply_to_a_different_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = AutoTasks::new(dir.path().join("tasks.json"));
    let mut slot = Some(workspace(&dir.path().join("project")));
    wait_loaded(&mut tool, &mut slot);
    tool.queue_created(slot.as_ref().unwrap(), EditTaskTrigger::Manual)
        .unwrap();
    slot.as_mut()
        .unwrap()
        .override_selection_duration(DurationUs::new(444_000).unwrap())
        .unwrap();
    let before = slot.as_ref().unwrap().manifest().clone();
    assert!(
        wait_run(&mut tool, &mut slot)
            .unwrap()
            .contains("project changed")
    );
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}

#[test]
fn automatic_recorded_cursor_uses_stored_pixels_and_follows_resize_and_rotation() {
    use gif_from_screen_domain::{
        AssetKind, PhysicalPoint, PhysicalPx, QuarterTurn, RasterEncoding,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let pixels = [0, 0, 255, 255, 255, 255, 0, 255];
    let id = current.active_project().assets().put(&pixels).unwrap();
    let descriptor = AssetDescriptor {
        id,
        byte_len: pixels.len() as u64,
        kind: AssetKind::OverlayImage {
            size: PhysicalSize::new(2, 1).unwrap(),
            encoding: RasterEncoding::Rgba8,
        },
    };
    let mut frame = current.manifest().timeline.frames[0].clone();
    frame.capture_metadata.cursor_visible = true;
    frame.capture_binding = gif_from_screen_domain::CaptureBinding::Original;
    frame.capture_metadata.cursor_asset = Some(id);
    frame.capture_metadata.cursor_position = Some(PhysicalPoint {
        x: PhysicalPx::new(1),
        y: PhysicalPx::new(1),
    });
    frame.transform.output_size = Some(PhysicalSize::new(16, 16).unwrap());
    frame.transform.rotation = QuarterTurn::Clockwise90;
    let mut canvas = current.manifest().canvas.clone();
    canvas.size = PhysicalSize::new(16, 16).unwrap();
    current
        .execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset { asset: descriptor },
                EditCommand::SetCanvas { canvas },
                EditCommand::ReplaceFrame {
                    frame_id: frame.id,
                    replacement: Box::new(frame),
                },
            ],
        })
        .unwrap();
    let before = current.manifest().clone();
    let chain = preset(vec![
        delay(200),
        EditingTaskAction::Annotation {
            request: AnnotationRequest {
                mode: AnnotationMode::RecordedCursor,
                size: PhysicalSize::new(16, 16).unwrap(),
                ..AnnotationRequest::default()
            },
        },
    ]);
    apply_task_chain(
        &mut current,
        &chain,
        EditTaskTrigger::ScreenRecording,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let preview = crate::editor_preview::render_frame_surface(
        current.active_project(),
        current.manifest().timeline.frames[0].id,
        1024 * 1024,
    )
    .unwrap();
    let pixel = |x: usize, y: usize| &preview.pixels()[(y * 16 + x) * 4..(y * 16 + x + 1) * 4];
    assert_eq!(pixel(12, 2), [0, 0, 255, 255]);
    assert_eq!(pixel(12, 4), [255, 255, 0, 255]);
    assert_eq!(pixel(0, 0), [255, 0, 0, 255]);
    assert!(current.undo().unwrap());
    let mut expected = before;
    expected.revision = current.manifest().revision;
    assert_eq!(current.manifest(), &expected);
}

#[test]
fn journal_failure_does_not_mutate_memory_or_allow_a_second_commit_without_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut current = workspace(&dir.path().join("project"));
    let before = current.manifest().clone();
    let journal = current.active_project().layout().journal.clone();
    if journal.exists() {
        std::fs::rename(&journal, journal.with_extension("test-backup")).unwrap();
    }
    std::fs::create_dir(&journal).unwrap();
    let error = apply_task_chain(
        &mut current,
        &preset(vec![delay(222), border()]),
        EditTaskTrigger::Manual,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.contains("journal"));
    assert_eq!(current.manifest(), &before);
    assert!(!current.can_undo());
    let retry = apply_task_chain(
        &mut current,
        &preset(vec![delay(222)]),
        EditTaskTrigger::Manual,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(retry.contains("previous journal write failed"), "{retry}");
    assert_eq!(current.manifest(), &before);
}

#[test]
fn close_waits_for_an_in_flight_settings_save_before_exiting() {
    use eframe::egui::{Context, RawInput, ViewportCommand, ViewportEvent, ViewportId};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let mut tool = AutoTasks::new(path.clone());
    wait_loaded(&mut tool, &mut None);
    let mut config = tool.draft.clone();
    config.presets.push(preset(vec![delay(123)]));
    config.active_preset = Some("Demo".to_owned());
    let previous = tool.snapshot.clone().unwrap();
    let store = tool.store.clone();
    let worker_config = config.clone();
    let (release, wait_for_release) = std::sync::mpsc::sync_channel(1);
    tool.settings_job
        .start("gfs-delayed-settings-save-test", move |_| {
            wait_for_release.recv().map_err(|error| error.to_string())?;
            store.save(&previous, worker_config)
        })
        .unwrap();
    let mut app = crate::GifFromScreenApp::default();
    std::mem::swap(&mut app.auto_tasks, &mut tool);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.project_library.is_active() {
        app.project_library.poll();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(!app.source_workers_active());
    assert!(!app.project_library.is_active());
    let context = Context::default();
    let mut closing = RawInput::default();
    closing
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .events
        .push(ViewportEvent::Close);
    let output = context.run(closing, |context| app.handle_worker_shutdown(context));
    assert!(
        output.viewport_output[&ViewportId::ROOT]
            .commands
            .contains(&ViewportCommand::CancelClose)
    );
    assert!(
        !output.viewport_output[&ViewportId::ROOT]
            .commands
            .contains(&ViewportCommand::Close)
    );
    assert_eq!(app.shutdown, crate::ShutdownState::WaitingForWorkers);
    assert!(!path.exists());
    release.send(()).unwrap();
    wait_loaded(&mut app.auto_tasks, &mut app.editor_workspace);
    let output = context.run(RawInput::default(), |context| {
        app.handle_worker_shutdown(context);
    });
    assert!(
        output.viewport_output[&ViewportId::ROOT]
            .commands
            .contains(&ViewportCommand::Close)
    );
    assert_eq!(AutoTaskStore::new(path).load().unwrap().config, config);
}
