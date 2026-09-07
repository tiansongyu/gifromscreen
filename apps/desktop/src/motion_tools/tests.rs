use super::*;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{DurationUs, FrameId, Rgba, UnixTimeMs};
use std::{thread, time::Instant};

fn workspace(root: &std::path::Path) -> EditorWorkspace {
    let project = create_blank_animation_project(
        root,
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(7),
            frame_id: FrameId::from_u128(1),
            app_version: "motion-ui-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            canvas: PhysicalSize::new(2, 1).unwrap(),
            background: Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            frame_duration: DurationUs::new(10_000).unwrap(),
            frame_limit_bytes: 1024,
        },
    )
    .unwrap();
    let mut workspace = EditorWorkspace::from_active(project, 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

fn ink_tool(workspace: &EditorWorkspace) -> MotionTools {
    let mut tool = MotionTools {
        mode: Mode::Cinemagraph,
        ..MotionTools::default()
    };
    tool.cine.begin(workspace).unwrap();
    let point = gif_from_screen_render::InkSample {
        position: gif_from_screen_render::InkPoint { x: 0.5, y: 0.5 },
        pressure: 0.5,
    };
    tool.cine.pointer_down(point).unwrap();
    tool.cine.pointer_up(point).unwrap();
    tool
}

#[test]
fn cinemagraph_cancel_retains_draft_and_success_returns_workspace_then_clears_draft() {
    let directory = tempfile::tempdir().unwrap();
    let mut slot = Some(workspace(&directory.path().join("ink.gfsproj")));
    let before = slot.as_ref().unwrap().manifest().clone();
    let mut tool = ink_tool(slot.as_ref().unwrap());
    tool.queue(slot.as_ref().unwrap()).unwrap();
    tool.cancel();
    assert!(wait(&mut tool, &mut slot).contains("cancelled"));
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
    assert!(tool.cine.is_active());
    assert_eq!(tool.cine.strokes().len(), 1);
    tool.queue(slot.as_ref().unwrap()).unwrap();
    assert!(wait(&mut tool, &mut slot).contains("applied to 1"));
    assert!(!tool.cine.is_active());
    assert!(tool.cine.strokes().is_empty());
    let workspace = slot.as_mut().unwrap();
    assert!(workspace.can_undo());
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, before.timeline);
    assert_eq!(workspace.manifest().assets, before.assets);
}

#[test]
fn failed_cinemagraph_returns_original_history_and_retains_ink_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let mut slot = Some(workspace(&directory.path().join("bad-ink.gfsproj")));
    let before = slot.as_ref().unwrap().manifest().clone();
    let mut tool = ink_tool(slot.as_ref().unwrap());
    let frame = &before.timeline.frames[0];
    let path = slot
        .as_ref()
        .unwrap()
        .active_project()
        .assets()
        .asset_path(frame.asset_id);
    std::fs::remove_file(path).unwrap();
    tool.queue(slot.as_ref().unwrap()).unwrap();
    assert!(wait(&mut tool, &mut slot).contains("did not complete"));
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
    assert!(tool.cine.is_active());
    assert_eq!(tool.cine.strokes().len(), 1);
    assert!(!tool.is_running());
}

fn wait(tool: &mut MotionTools, slot: &mut Option<EditorWorkspace>) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(notice) = tool.poll(slot)
            && !tool.is_running()
        {
            return notice;
        }
        assert!(
            Instant::now() < deadline,
            "motion task failed to return the workspace"
        );
        thread::yield_now();
    }
}

#[test]
fn pending_motion_is_exclusive_and_cancellation_never_loans_the_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let mut slot = Some(workspace(&directory.path().join("project")));
    let before = slot.as_ref().unwrap().manifest().clone();
    let mut tool = MotionTools::default();
    tool.queue(slot.as_ref().unwrap()).unwrap();
    assert!(tool.is_running());
    tool.cancel();
    assert!(tool.poll(&mut slot).unwrap().contains("cancelled"));
    assert!(!tool.is_running());
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}

#[test]
fn successful_background_motion_returns_the_same_workspace_with_one_undo_entry() {
    let directory = tempfile::tempdir().unwrap();
    let mut slot = Some(workspace(&directory.path().join("project")));
    let before = slot.as_ref().unwrap().manifest().clone();
    let mut tool = MotionTools {
        width: 1,
        height: 1,
        ..MotionTools::default()
    };
    tool.queue(slot.as_ref().unwrap()).unwrap();
    assert!(tool.poll(&mut slot).is_none());
    assert!(slot.is_none());
    let notice = wait(&mut tool, &mut slot);
    assert!(notice.contains("applied to 1 frames"));
    let workspace = slot.as_mut().unwrap();
    assert_eq!(workspace.manifest().project_id, before.project_id);
    assert!(workspace.undo().unwrap());
    let mut expected = before;
    expected.revision = workspace.manifest().revision;
    assert_eq!(workspace.manifest(), &expected);
}

#[test]
fn failed_background_motion_restores_workspace_and_existing_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut current = workspace(&directory.path().join("project"));
    current
        .override_selection_duration(DurationUs::new(20_000).unwrap())
        .unwrap();
    let before = current.manifest().clone();
    let mut slot = Some(current);
    let mut tool = MotionTools::default(); // Deliberately larger than the 2x1 canvas.
    tool.queue(slot.as_ref().unwrap()).unwrap();
    tool.poll(&mut slot);
    let notice = wait(&mut tool, &mut slot);
    assert!(notice.contains("did not complete"));
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
    assert!(slot.as_ref().unwrap().can_undo());
}

#[test]
fn panicking_worker_cannot_drop_the_loaned_workspace_or_its_undo_history() {
    let directory = tempfile::tempdir().unwrap();
    let current = workspace(&directory.path().join("project"));
    let before = current.manifest().clone();
    let loan = Arc::new(Mutex::new(Some(current)));
    let worker_loan = Arc::clone(&loan);
    let mut tool = MotionTools {
        loan: Some(loan),
        ..MotionTools::default()
    };
    tool.task
        .start("motion-panic-test", move |_| {
            let _held = worker_loan.lock().unwrap();
            panic!("simulated motion worker failure");
        })
        .unwrap();
    let mut slot = None;
    assert!(wait(&mut tool, &mut slot).contains("did not complete"));
    assert_eq!(slot.as_ref().unwrap().manifest(), &before);
}
