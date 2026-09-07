//! Expanded-image payloads require the durable schema-4 gate, not merely v3.

use std::{fs, io};

use gif_from_screen_domain::{
    EditCommand, FrameClip, FrameRenderStep, ImageBorderStyle, ImageShadowStyle, IndexedFrame,
    ProjectRevision, SignedEdgeWidths,
};

use super::{
    ActiveProject, LockPolicy, ProjectError,
    schema3_migration::{assert_upgrade_roundtrip, project},
    write_manifest,
};
use crate::{JournalRecord, JournalStopReason, journal};

fn image_steps() -> [FrameRenderStep; 2] {
    [
        FrameRenderStep::ImageBorder {
            style: ImageBorderStyle {
                widths: SignedEdgeWidths {
                    left_milli: -1_500,
                    ..SignedEdgeWidths::default()
                },
                ..ImageBorderStyle::default()
            },
        },
        FrameRenderStep::ImageShadow {
            style: ImageShadowStyle::default(),
        },
    ]
}

fn expanded_frame(project: &ActiveProject, step: FrameRenderStep) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![FrameRenderStep::composite(1), step];
    frame
}

#[test]
fn schema_three_upgrade_stamps_old_state_and_keeps_v4_through_undo_reopen_redo() {
    for step in image_steps() {
        let directory = tempfile::tempdir().unwrap();
        let project = project(directory.path(), 3);
        let frame = expanded_frame(&project, step);
        let command = EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        };
        assert_upgrade_roundtrip(directory.path(), project, command, 4);
    }
}

#[test]
fn schema_three_journal_refuses_all_nested_image_frame_payloads_before_replay() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(&directory.path().join("source"), 3);
    for (step_index, step) in image_steps().into_iter().enumerate() {
        let frame = expanded_frame(&project, step);
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
        for (command_index, command) in commands.into_iter().enumerate() {
            let command = EditCommand::Compound {
                commands: vec![command],
            };
            assert_eq!(command.required_schema_version(), 4);
            let path = directory
                .path()
                .join(format!("unstamped-{step_index}-{command_index}.ndjson"));
            let record =
                JournalRecord::new(ProjectRevision::new(1), ProjectRevision::new(2), command)
                    .unwrap();
            journal::append(&path, &record).unwrap();
            let before = project.manifest().clone();
            let recovered = journal::recover(before.clone(), &path).unwrap();
            assert_eq!(recovered.manifest, before);
            assert_eq!(
                recovered.report.stop_reason,
                Some(JournalStopReason::SchemaMismatch {
                    line: 1,
                    snapshot_schema: 3,
                    required_schema: 4
                })
            );
        }
    }
}

#[test]
fn invalid_image_parameters_cannot_upgrade_or_append_to_a_v3_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 3);
    let before = project.manifest().clone();
    let manifest = fs::read(&project.layout().manifest).unwrap();
    let journal = fs::read(&project.layout().journal).unwrap();
    let invalid = FrameRenderStep::ImageShadow {
        style: ImageShadowStyle {
            opacity_basis_points: 10_001,
            ..ImageShadowStyle::default()
        },
    };
    let frame = expanded_frame(&project, invalid);
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
fn ambiguous_v4_stamp_requires_recovery_and_never_publishes_image_effects() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 3);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    let error = project
        .stamp_schema_upgrade_with(4, |path, manifest| {
            write_manifest(path, manifest)?;
            Err(ProjectError::io(
                "injected post-publication schema4 failure",
                path,
                io::Error::other("injected"),
            ))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectError::SchemaUpgradeFailed {
            from_version: 3,
            to_version: 4,
            ..
        }
    ));
    assert_eq!(project.manifest(), &before);
    let frame = expanded_frame(&project, image_steps()[0].clone());
    assert!(matches!(
        project.commit(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame)
        }),
        Err(ProjectError::RequiresRecovery)
    ));
    assert!(matches!(
        project.checkpoint(),
        Err(ProjectError::RequiresRecovery)
    ));
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    let mut expected = before;
    expected.schema_version = 4;
    assert_eq!(opened.project.manifest(), &expected);
}
