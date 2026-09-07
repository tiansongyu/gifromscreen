//! The immutable freeze reference cannot appear before a durable schema-6 gate.

use std::{fs, io};

use super::{
    ActiveProject, LockPolicy, ProjectError,
    schema3_migration::{assert_upgrade_roundtrip, project},
    write_manifest,
};
use crate::{JournalRecord, JournalStopReason, journal};
use gif_from_screen_domain::{
    EditCommand, FrameClip, FrameRenderStep, IndexedFrame, PhysicalRect, ProjectRevision,
};

fn frozen(project: &ActiveProject) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::FreezeRegion {
            baseline_asset: frame.asset_id,
            baseline_size: project.manifest().canvas.size,
            region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
            invert: false,
        },
    ];
    frame
}

#[test]
fn freeze_upgrade_stamps_old_committed_state_and_undo_redo_reopen_keep_schema_six() {
    for version in [4, 5] {
        let directory = tempfile::tempdir().unwrap();
        let project = project(directory.path(), version);
        let frame = frozen(&project);
        let command = EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        };
        assert_upgrade_roundtrip(directory.path(), project, command, 6);
    }
}

#[test]
fn old_headers_refuse_every_nested_freeze_frame_payload_before_replay() {
    for version in [4, 5] {
        let directory = tempfile::tempdir().unwrap();
        let project = project(&directory.path().join("project"), version);
        let frame = frozen(&project);
        let commands = [
            EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame.clone()),
            },
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
                    snapshot_schema: version,
                    required_schema: 6,
                })
            );
        }
    }
}

#[test]
fn invalid_baseline_cannot_stamp_or_append_to_a_v5_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 5);
    let before = project.manifest().clone();
    let manifest = fs::read(&project.layout().manifest).unwrap();
    let journal = fs::read(&project.layout().journal).unwrap();
    let mut frame = frozen(&project);
    if let FrameRenderStep::FreezeRegion { baseline_asset, .. } = &mut frame.render_steps[1] {
        *baseline_asset = gif_from_screen_domain::AssetId::from_digest([99; 32]);
    }
    assert!(
        project
            .commit(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame)
            })
            .is_err()
    );
    assert_eq!(project.manifest(), &before);
    assert_eq!(fs::read(&project.layout().manifest).unwrap(), manifest);
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
}

#[test]
fn ambiguous_schema_six_stamp_requires_recovery_without_publishing_freeze_pixels() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 5);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    let error = project
        .stamp_schema_upgrade_with(6, |path, manifest| {
            write_manifest(path, manifest)?;
            Err(ProjectError::io(
                "injected v6 post-publication stamp failure",
                path,
                io::Error::other("injected"),
            ))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectError::SchemaUpgradeFailed {
            from_version: 5,
            to_version: 6,
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
    let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(reopened.journal_recovery.is_clean());
    let mut expected = before;
    expected.schema_version = 6;
    assert_eq!(reopened.project.manifest(), &expected);
}
