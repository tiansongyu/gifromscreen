//! Typed PM resources, including asset-only registrations, require a durable v7 gate.

use std::{fs, io};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, EditCommand, FrameClip, FrameId, FrameRenderStep, IndexedFrame,
    PhysicalSize, ProjectRevision,
};

use super::{
    ActiveProject, LockPolicy, ProjectError,
    schema3_migration::{assert_upgrade_roundtrip, project},
    write_manifest,
};
use crate::{AssetCheck, JournalRecord, JournalStopReason, journal};

fn snapshot(project: &ActiveProject) -> AssetDescriptor {
    // Synthetic container tests persistence only; no claim of WPF-rendered pixels.
    let mut bytes = b"GFSPM8\0".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&[3, 6, 9, 128].repeat(4));
    AssetDescriptor {
        id: project.assets().put(&bytes).unwrap(),
        byte_len: u64::try_from(bytes.len()).unwrap(),
        kind: AssetKind::PremultipliedSnapshot {
            size: PhysicalSize::new(2, 2).unwrap(),
            format_version: 1,
        },
    }
}

fn authored_frame(project: &ActiveProject, snapshot: &AssetDescriptor) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::CinemagraphOverlay {
            snapshot_asset: snapshot.id,
            snapshot_size: project.manifest().canvas.size,
        },
    ];
    frame
}

#[test]
fn snapshot_asset_only_registration_stamps_old_state_and_keeps_v7_through_undo_recovery() {
    for version in 1..=6 {
        let directory = tempfile::tempdir().unwrap();
        let project = project(directory.path(), version);
        let asset = snapshot(&project);
        let command = EditCommand::RegisterAsset { asset };
        assert_eq!(command.required_schema_version(), 7);
        assert_upgrade_roundtrip(directory.path(), project, command, 7);
    }
}

#[test]
fn asset_and_cinemagraph_step_publish_in_one_revision_and_reopen_after_undo_redo() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 6);
    let asset = snapshot(&project);
    let frame = authored_frame(&project, &asset);
    let command = EditCommand::Compound {
        commands: vec![
            EditCommand::RegisterAsset { asset },
            EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            },
        ],
    };
    assert_upgrade_roundtrip(directory.path(), project, command, 7);
    let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent)
        .unwrap()
        .project;
    assert!(
        reopened
            .validate_assets(AssetCheck::FullDigest)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn old_snapshots_refuse_asset_registration_and_every_nested_snapshot_frame_payload() {
    for version in [4, 5, 6] {
        let directory = tempfile::tempdir().unwrap();
        let project = project(&directory.path().join("project"), version);
        let asset = snapshot(&project);
        let frame = authored_frame(&project, &asset);
        let commands = [
            EditCommand::RegisterAsset { asset },
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
                    required_schema: 7
                })
            );
        }
    }
}

#[test]
fn invalid_snapshot_registration_and_raw_recording_misuse_never_upgrade_or_append() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 6);
    let asset = snapshot(&project);
    let before = project.manifest().clone();
    let manifest = fs::read(&project.layout().manifest).unwrap();
    let journal = fs::read(&project.layout().journal).unwrap();
    for invalid in [
        AssetDescriptor {
            byte_len: asset.byte_len - 1,
            ..asset.clone()
        },
        AssetDescriptor {
            kind: AssetKind::PremultipliedSnapshot {
                size: PhysicalSize::new(2, 2).unwrap(),
                format_version: 2,
            },
            ..asset.clone()
        },
    ] {
        assert!(
            project
                .commit(EditCommand::RegisterAsset { asset: invalid })
                .is_err()
        );
        assert_eq!(project.manifest(), &before);
        assert_eq!(fs::read(&project.layout().manifest).unwrap(), manifest);
        assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    }
    let mut raw_frame = project.manifest().timeline.frames[0].clone();
    raw_frame.id = FrameId::from_u128(2);
    raw_frame.asset_id = asset.id;
    assert!(matches!(
        project.commit_recording_append(Some(asset), raw_frame),
        Err(ProjectError::InvalidRecordingMutation(_))
    ));
    assert_eq!(project.manifest(), &before);
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
}

#[test]
fn ambiguous_v7_stamp_blocks_registration_and_recovers_only_old_registered_assets() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 6);
    let asset = snapshot(&project);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    let error = project
        .stamp_schema_upgrade_with(7, |path, manifest| {
            write_manifest(path, manifest)?;
            Err(ProjectError::io(
                "injected v7 post-publication stamp failure",
                path,
                io::Error::other("injected"),
            ))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectError::SchemaUpgradeFailed {
            from_version: 6,
            to_version: 7,
            ..
        }
    ));
    assert_eq!(project.manifest(), &before);
    assert!(matches!(
        project.commit(EditCommand::RegisterAsset { asset }),
        Err(ProjectError::RequiresRecovery)
    ));
    assert!(matches!(
        project.checkpoint(),
        Err(ProjectError::RequiresRecovery)
    ));
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    drop(project);
    let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(reopened.journal_recovery.is_clean());
    let mut expected = before;
    expected.schema_version = 7;
    assert_eq!(reopened.project.manifest(), &expected);
}
