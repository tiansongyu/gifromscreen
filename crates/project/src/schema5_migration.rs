//! A WPF paint boundary is an explicit, durable upgrade, never a v4 reinterpretation.

use std::{fs, io};

use gif_from_screen_domain::{
    CompositePrecision, EditCommand, FrameClip, FrameRenderStep, IndexedFrame, ProjectRevision,
};

use super::{
    ActiveProject, LockPolicy, ProjectError,
    schema3_migration::{assert_upgrade_roundtrip, project},
    write_manifest,
};
use crate::{JournalRecord, JournalStopReason, journal};

fn authored_frame(project: &ActiveProject) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Composite {
            stage_id: 2,
            precision: CompositePrecision::WpfPbgra8PngV1,
        },
    ];
    frame
}

#[test]
fn paint_upgrade_stamps_old_v4_state_and_undo_reopen_keep_schema_five() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 4);
    let frame = authored_frame(&project);
    let command = EditCommand::ReplaceFrame {
        frame_id: frame.id,
        replacement: Box::new(frame),
    };
    assert_upgrade_roundtrip(directory.path(), project, command, 5);
}

#[test]
fn old_snapshot_refuses_new_paint_precision_in_every_frame_payload_before_replay() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(&directory.path().join("project"), 4);
    let frame = authored_frame(&project);
    let commands = [
        EditCommand::InsertFrames {
            index: 1,
            frames: vec![frame.clone()],
        },
        EditCommand::RestoreFrames {
            frames: vec![IndexedFrame {
                index: 1,
                frame: frame.clone(),
            }],
        },
        EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame.clone()),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            }),
            overlay_tracks: Vec::new(),
        },
    ];
    for (position, command) in commands.into_iter().enumerate() {
        let path = directory.path().join(format!("journal-{position}.ndjson"));
        let record = JournalRecord::new(
            ProjectRevision::new(1),
            ProjectRevision::new(2),
            EditCommand::Compound {
                commands: vec![command],
            },
        )
        .unwrap();
        journal::append(&path, &record).unwrap();
        let before = project.manifest().clone();
        let recovered = journal::recover(before.clone(), &path).unwrap();
        assert_eq!(recovered.manifest, before);
        assert_eq!(
            recovered.report.stop_reason,
            Some(JournalStopReason::SchemaMismatch {
                line: 1,
                snapshot_schema: 4,
                required_schema: 5
            })
        );
    }
}

#[test]
fn invalid_first_precision_cannot_stamp_or_append_to_the_legacy_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 4);
    let before = project.manifest().clone();
    let manifest_bytes = fs::read(&project.layout().manifest).unwrap();
    let journal_bytes = fs::read(&project.layout().journal).unwrap();
    let mut frame = authored_frame(&project);
    frame.render_steps.remove(0);
    assert!(
        project
            .commit(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame)
            })
            .is_err()
    );
    assert_eq!(project.manifest(), &before);
    assert_eq!(
        fs::read(&project.layout().manifest).unwrap(),
        manifest_bytes
    );
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal_bytes);
}

#[test]
fn ambiguous_v5_stamp_requires_recovery_without_publishing_any_new_paint() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 4);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    let error = project
        .stamp_schema_upgrade_with(5, |path, manifest| {
            write_manifest(path, manifest)?;
            Err(ProjectError::io(
                "injected post-publication v5 stamp failure",
                path,
                io::Error::other("injected"),
            ))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectError::SchemaUpgradeFailed {
            from_version: 4,
            to_version: 5,
            ..
        }
    ));
    assert_eq!(project.manifest(), &before);
    assert!(matches!(
        project.checkpoint(),
        Err(ProjectError::RequiresRecovery)
    ));
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    let mut expected = before;
    expected.schema_version = 5;
    assert_eq!(opened.project.manifest(), &expected);
}
