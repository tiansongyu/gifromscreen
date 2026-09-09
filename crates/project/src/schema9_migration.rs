//! Persisted vector V2 requires an explicit, durable schema-nine gate.

use std::{fs, io};

use gif_from_screen_domain::{
    BlendMode, CompositePrecision, EditCommand, FrameClip, FrameId, FrameOverlayCell,
    FrameOverlayMark, FrameRenderStep, IndexedFrame, OverlayContent, OverlayId, OverlayTrack,
    ProjectRevision, TrackId, VectorShape,
};

use super::super::schema3_migration::{assert_upgrade_roundtrip, project};
use super::{
    ActiveProject, LockPolicy, ProjectError, assert_recovery_gate, old_state_at_version,
    read_manifest, write_manifest,
};
use crate::{AssetCheck, JournalRecord, JournalStopReason, journal};

fn staged_frame(project: &ActiveProject) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Composite {
            stage_id: 2,
            precision: CompositePrecision::VectorCanvasPbgra8PngV2,
        },
    ];
    frame
}

fn track() -> OverlayTrack {
    let mut cell = FrameOverlayCell::whole(
        FrameId::from_u128(1),
        1,
        vec![FrameOverlayMark {
            id: OverlayId::from_u128(901),
            z_index: 0,
            content: OverlayContent::VectorShape {
                shape: VectorShape::wpf_v2(),
            },
        }],
    );
    cell.stage = Some(2);
    OverlayTrack {
        id: TrackId::from_u128(901),
        name: "Saved V2 用户 {name}".into(),
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Normal,
        annotation: None,
        annotation_scope: None,
        frame_cells: Some(vec![cell]),
        items: Vec::new(),
    }
}

fn new_payload(project: &ActiveProject) -> EditCommand {
    EditCommand::Compound {
        commands: vec![
            EditCommand::ReplaceFrame {
                frame_id: FrameId::from_u128(1),
                replacement: Box::new(staged_frame(project)),
            },
            EditCommand::UpsertOverlayTrack { track: track() },
        ],
    }
}

#[test]
fn schema9_stamps_previous_state_then_replays_v2_and_keeps_upgrade_through_undo_redo() {
    for version in [1, 7, 8] {
        let directory = tempfile::tempdir().unwrap();
        let project = project(directory.path(), version);
        let command = new_payload(&project);
        assert_eq!(command.required_schema_version(), 9);
        // Checks the stamped snapshot contains only the previous committed
        // visual state, then journal replay, undo, compact/reopen and redo.
        assert_upgrade_roundtrip(directory.path(), project, command, 9);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(opened.journal_recovery.is_clean());
        assert!(opened.asset_issues.is_empty());
        assert!(
            opened
                .project
                .validate_assets(AssetCheck::FullDigest)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            opened.project.manifest().timeline.overlay_tracks,
            vec![track()]
        );
    }
}

#[test]
fn empty_v2_boundary_alone_requires_schema9_and_survives_undo_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 8);
    let frame = staged_frame(&project);
    let command = EditCommand::ReplaceFrame {
        frame_id: frame.id,
        replacement: Box::new(frame),
    };
    assert_eq!(command.required_schema_version(), 9);
    assert_upgrade_roundtrip(directory.path(), project, command, 9);
}

#[test]
fn invalid_binding_and_late_failure_change_neither_disk_nor_schema_nor_assets() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 8);
    let before = project.manifest().clone();
    let disk = fs::read(&project.layout().manifest).unwrap();
    let journal = fs::read(&project.layout().journal).unwrap();
    let source = project
        .assets()
        .read(before.timeline.frames[0].asset_id)
        .unwrap();
    let asset_count = fs::read_dir(&project.layout().assets).unwrap().count();
    let mut non_normal = track();
    non_normal.blend_mode = BlendMode::Screen;
    for command in [
        EditCommand::UpsertOverlayTrack { track: track() },
        EditCommand::Compound {
            commands: vec![
                new_payload(&project),
                EditCommand::UpsertOverlayTrack { track: non_normal },
            ],
        },
        EditCommand::Compound {
            commands: vec![
                new_payload(&project),
                EditCommand::RemoveOverlayTrack {
                    track_id: TrackId::from_u128(999),
                },
            ],
        },
    ] {
        assert!(project.commit(command).is_err());
        assert_eq!(project.manifest(), &before);
        assert_eq!(fs::read(&project.layout().manifest).unwrap(), disk);
        assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
        assert_eq!(
            project
                .assets()
                .read(before.timeline.frames[0].asset_id)
                .unwrap(),
            source
        );
        assert_eq!(
            fs::read_dir(&project.layout().assets).unwrap().count(),
            asset_count
        );
        assert!(!project.write_requires_recovery);
    }
}

#[test]
fn crash_after_schema9_stamp_before_payload_retains_only_previous_pixels_and_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 8);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    project.stamp_schema_upgrade(9).unwrap();
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    assert_eq!(
        read_manifest(&project.layout().manifest).unwrap(),
        old_state_at_version(&before, 9)
    );
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(opened.journal_recovery.already_snapshotted_records, 1);
    assert_eq!(opened.journal_recovery.replayed_records, 0);
    assert_eq!(opened.project.manifest(), &old_state_at_version(&before, 9));
    assert_eq!(
        opened
            .project
            .assets()
            .read(before.timeline.frames[0].asset_id)
            .unwrap(),
        [127; 16]
    );
}

fn nested_commands(project: &ActiveProject) -> Vec<EditCommand> {
    let frame = staged_frame(project);
    vec![
        EditCommand::UpsertOverlayTrack { track: track() },
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: track(),
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
        EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::SetFrameDurations {
                changes: Vec::new(),
            }),
            overlay_tracks: vec![track()],
        },
        EditCommand::Compound {
            commands: vec![new_payload(project)],
        },
    ]
}

#[test]
fn schema8_rejects_every_nested_v2_record_before_replay_or_already_snapshotted_skip() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(&directory.path().join("project"), 8);
    for (index, command) in nested_commands(&project).into_iter().enumerate() {
        assert_eq!(command.required_schema_version(), 9);
        let path = directory.path().join(format!("journal-{index}.ndjson"));
        let record =
            JournalRecord::new(ProjectRevision::new(1), ProjectRevision::new(2), command).unwrap();
        journal::append(&path, &record).unwrap();
        for revision in [ProjectRevision::new(1), ProjectRevision::new(2)] {
            let mut snapshot = project.manifest().clone();
            snapshot.revision = revision;
            let recovered = journal::recover(snapshot.clone(), &path).unwrap();
            assert_eq!(recovered.manifest, snapshot);
            assert_eq!(recovered.report.replayed_records, 0);
            assert_eq!(recovered.report.already_snapshotted_records, 0);
            assert_eq!(
                recovered.report.stop_reason,
                Some(JournalStopReason::SchemaMismatch {
                    line: 1,
                    snapshot_schema: 8,
                    required_schema: 9,
                })
            );
        }
    }
}

#[test]
fn correctly_stamped_but_invalid_hidden_v2_record_is_rejected_and_blocks_writes() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 8);
    project.stamp_schema_upgrade(9).unwrap();
    let before = project.manifest().clone();
    let record = JournalRecord::new(
        before.revision,
        before.revision.next().unwrap(),
        EditCommand::UpsertOverlayTrack { track: track() },
    )
    .unwrap();
    journal::append(&project.layout().journal, &record).unwrap();
    drop(project);
    let mut opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(matches!(
        opened.journal_recovery.stop_reason,
        Some(JournalStopReason::CommandRejected { .. })
    ));
    assert_eq!(opened.project.manifest(), &before);
    assert!(matches!(
        opened.project.commit(super::legacy_edit()),
        Err(ProjectError::RequiresJournalRepair)
    ));
}

#[test]
fn failed_schema9_stamp_requires_recovery_and_never_appends_new_payload() {
    for published in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut project = project(directory.path(), 8);
        let before = project.manifest().clone();
        let journal = fs::read(&project.layout().journal).unwrap();
        let error = project
            .stamp_schema_upgrade_with(9, |path, manifest| {
                if published {
                    write_manifest(path, manifest)?;
                }
                Err(ProjectError::io(
                    "injected schema9 stamp failure",
                    path,
                    io::Error::other("injected"),
                ))
            })
            .unwrap_err();
        assert!(matches!(
            error,
            ProjectError::SchemaUpgradeFailed {
                from_version: 8,
                to_version: 9,
                ..
            }
        ));
        assert_eq!(project.manifest(), &before);
        assert_recovery_gate(&mut project);
        assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
        drop(project);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(opened.journal_recovery.is_clean());
        assert_eq!(
            opened.project.manifest(),
            &old_state_at_version(&before, if published { 9 } else { 8 })
        );
    }
}

#[test]
fn append_failure_after_schema9_stamp_preserves_previous_visual_state_and_sticky_gate() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 8);
    let before = project.manifest().clone();
    let edit = new_payload(&project);
    let preserved = directory.path().join("preserved-journal.ndjson");
    fs::rename(&project.layout().journal, &preserved).unwrap();
    fs::create_dir(&project.layout().journal).unwrap();
    assert!(matches!(
        project.commit(edit),
        Err(ProjectError::JournalCommitFailed { .. })
    ));
    assert_eq!(project.manifest(), &old_state_at_version(&before, 9));
    assert_eq!(
        read_manifest(&project.layout().manifest).unwrap(),
        old_state_at_version(&before, 9)
    );
    assert_recovery_gate(&mut project);
    fs::remove_dir(&project.layout().journal).unwrap();
    fs::rename(&preserved, &project.layout().journal).unwrap();
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(opened.project.manifest(), &old_state_at_version(&before, 9));
}
