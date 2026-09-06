//! Persistence boundaries for opt-in, irreversible project-format upgrades.

use std::{fs, io};

use gif_from_screen_domain::{
    BlendMode, Canvas, CanvasBackground, ColorSpace, EditCommand, OverlayTrack, PhysicalSize,
    ProjectId, ProjectManifest, ProjectRevision, TrackId, UnixTimeMs,
};

use super::{ActiveProject, LockPolicy, ProjectError, read_manifest, write_manifest};
use crate::{JournalRecord, JournalStopReason, journal};

fn legacy_manifest() -> ProjectManifest {
    let mut manifest = ProjectManifest::new(
        ProjectId::from_u128(123),
        "schema-migration-test",
        UnixTimeMs::new(1),
        Canvas {
            size: PhysicalSize::new(2, 2).unwrap(),
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    manifest.schema_version = 1;
    manifest.validate().unwrap();
    manifest
}

fn frame_owned_track() -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(1),
        name: "Frame-owned annotations".to_owned(),
        annotation: None,
        annotation_scope: None,
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        frame_cells: Some(Vec::new()),
    }
}

fn new_payload() -> EditCommand {
    EditCommand::UpsertOverlayTrack {
        track: frame_owned_track(),
    }
}

fn legacy_edit() -> EditCommand {
    EditCommand::SetCanvas {
        canvas: Canvas {
            size: PhysicalSize::new(3, 2).unwrap(),
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    }
}

fn old_state_at_version(project: &ProjectManifest, version: u32) -> ProjectManifest {
    let mut expected = project.clone();
    expected.schema_version = version;
    expected
}

#[test]
fn new_payload_stamps_only_committed_state_before_journaling_and_undo_keeps_v2() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut project = ActiveProject::create(root, legacy_manifest()).unwrap();
    project.commit(legacy_edit()).unwrap();
    let previous = project.manifest().clone();
    let old_journal = fs::read(&project.layout().journal).unwrap();
    assert_eq!(
        read_manifest(&project.layout().manifest).unwrap().revision,
        ProjectRevision::ZERO
    );

    let receipt = project.commit(new_payload()).unwrap();
    assert_eq!(project.manifest().schema_version, 2);
    assert_eq!(project.manifest().revision, ProjectRevision::new(2));
    assert_eq!(
        read_manifest(&project.layout().manifest).unwrap(),
        old_state_at_version(&previous, 2)
    );
    assert!(
        fs::read(&project.layout().journal)
            .unwrap()
            .starts_with(&old_journal)
    );
    let after = project.manifest().clone();
    drop(project);

    let opened = ActiveProject::open(root, LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(opened.journal_recovery.already_snapshotted_records, 1);
    assert_eq!(opened.journal_recovery.replayed_records, 1);
    assert_eq!(opened.project.manifest(), &after);
    let mut project = opened.project;
    let redo = project.commit(receipt.inverse).unwrap().inverse;
    let mut undone = old_state_at_version(&previous, 2);
    undone.revision = ProjectRevision::new(3);
    assert_eq!(project.manifest(), &undone);
    project.checkpoint_and_compact().unwrap();
    drop(project);
    let mut reopened = ActiveProject::open(root, LockPolicy::FailIfPresent)
        .unwrap()
        .project;
    assert_eq!(reopened.manifest(), &undone);
    reopened.commit(redo).unwrap();
    assert_eq!(reopened.manifest().schema_version, 2);
    assert_eq!(reopened.manifest().timeline, after.timeline);
}

#[test]
fn invalid_edit_is_rejected_before_any_format_upgrade() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    let before = project.manifest().clone();
    let disk = fs::read(&project.layout().manifest).unwrap();
    let mut invalid = frame_owned_track();
    invalid.name.clear();
    let late_invalid = EditCommand::Compound {
        commands: vec![
            new_payload(),
            EditCommand::RemoveOverlayTrack {
                track_id: TrackId::from_u128(99),
            },
        ],
    };
    for command in [
        EditCommand::UpsertOverlayTrack { track: invalid },
        late_invalid,
    ] {
        assert!(project.commit(command).is_err());
        assert_eq!(project.manifest(), &before);
        assert_eq!(fs::read(&project.layout().manifest).unwrap(), disk);
        assert!(fs::read(&project.layout().journal).unwrap().is_empty());
        assert!(!project.write_requires_recovery);
    }
    project.commit(legacy_edit()).unwrap();
    assert_eq!(project.manifest().schema_version, 1);
}

#[test]
fn exit_after_format_stamp_before_new_payload_recovers_only_previous_visual_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    project.commit(legacy_edit()).unwrap();
    let previous = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    project.stamp_schema_upgrade(2).unwrap();
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert_eq!(
        opened.project.manifest(),
        &old_state_at_version(&previous, 2)
    );
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(opened.journal_recovery.already_snapshotted_records, 1);
    assert_eq!(opened.journal_recovery.replayed_records, 0);
}

fn assert_recovery_gate(project: &mut ActiveProject) {
    assert!(matches!(
        project.commit(legacy_edit()),
        Err(ProjectError::RequiresRecovery)
    ));
    assert!(matches!(
        project.checkpoint(),
        Err(ProjectError::RequiresRecovery)
    ));
    assert!(matches!(
        project.checkpoint_and_compact(),
        Err(ProjectError::RequiresRecovery)
    ));
    assert!(matches!(
        project.stamp_schema_upgrade(2),
        Err(ProjectError::RequiresRecovery)
    ));
}

#[test]
fn failed_format_stamp_blocks_writes_even_when_the_new_snapshot_is_already_visible() {
    for published in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
        project.commit(legacy_edit()).unwrap();
        let previous = project.manifest().clone();
        let old_journal = fs::read(&project.layout().journal).unwrap();
        let error = project
            .stamp_schema_upgrade_with(2, |path, manifest| {
                if published {
                    write_manifest(path, manifest)?;
                }
                // Simulates both failure before rename and an error returned after
                // the replacement is visible but directory durability is uncertain.
                Err(ProjectError::io(
                    "injected schema stamp failure",
                    path,
                    io::Error::other("injected"),
                ))
            })
            .unwrap_err();
        assert!(matches!(
            error,
            ProjectError::SchemaUpgradeFailed {
                from_version: 1,
                to_version: 2,
                ..
            }
        ));
        assert_eq!(project.manifest(), &previous);
        assert_recovery_gate(&mut project);
        assert_eq!(fs::read(&project.layout().journal).unwrap(), old_journal);
        drop(project);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(
            opened.project.manifest(),
            &old_state_at_version(&previous, if published { 2 } else { 1 })
        );
        assert!(opened.journal_recovery.is_clean());
    }
}

#[test]
fn append_failure_after_stamp_does_not_revert_format_or_publish_the_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    let previous = project.manifest().clone();
    let preserved_journal = directory.path().join("original-journal.ndjson");
    fs::rename(&project.layout().journal, &preserved_journal).unwrap();
    fs::create_dir(&project.layout().journal).unwrap();
    assert!(matches!(
        project.commit(new_payload()),
        Err(ProjectError::JournalCommitFailed { .. })
    ));
    assert_eq!(project.manifest(), &old_state_at_version(&previous, 2));
    assert_eq!(
        read_manifest(&project.layout().manifest).unwrap(),
        old_state_at_version(&previous, 2)
    );
    assert_recovery_gate(&mut project);
    fs::remove_dir(&project.layout().journal).unwrap();
    fs::rename(&preserved_journal, &project.layout().journal).unwrap();
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert_eq!(
        opened.project.manifest(),
        &old_state_at_version(&previous, 2)
    );
    assert!(opened.journal_recovery.is_clean());
}

#[test]
fn recovery_requires_a_durable_v2_snapshot_for_every_nested_new_payload() {
    let commands = [
        new_payload(),
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: frame_owned_track(),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::SetFrameDurations {
                changes: Vec::new(),
            }),
            overlay_tracks: vec![frame_owned_track()],
        },
        EditCommand::Compound {
            commands: vec![EditCommand::Compound {
                commands: vec![new_payload()],
            }],
        },
    ];
    for command in commands {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.ndjson");
        let record =
            JournalRecord::new(ProjectRevision::ZERO, ProjectRevision::new(1), command).unwrap();
        journal::append(&path, &record).unwrap();
        for revision in [ProjectRevision::ZERO, ProjectRevision::new(1)] {
            let mut snapshot = legacy_manifest();
            snapshot.revision = revision;
            let recovered = journal::recover(snapshot.clone(), &path).unwrap();
            assert_eq!(recovered.manifest, snapshot);
            assert_eq!(recovered.report.replayed_records, 0);
            assert_eq!(recovered.report.already_snapshotted_records, 0);
            assert_eq!(
                recovered.report.stop_reason,
                Some(JournalStopReason::SchemaMismatch {
                    line: 1,
                    snapshot_schema: 1,
                    required_schema: 2,
                })
            );
        }
    }
}

// Literal old checksum payload, not reserialized through the new domain model.
// Any accidentally emitted new optional field would invalidate this record.
const LEGACY_CHECKSUM_PAYLOAD: &str = concat!(
    r#"{"format_version":1,"sequence":1,"base_revision":0,"result_revision":1,"command":"#,
    r#"{"type":"upsert_overlay_track","track":{"id":"00000000000000000000000000000001","name":"Legacy overlay","visible":true,"opacity":255,"blend_mode":"normal","items":[]}}}"#,
);

#[test]
fn unchanged_v1_overlay_journal_checksum_and_replay_survive_the_new_reader() {
    let directory = tempfile::tempdir().unwrap();
    let project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    let before = fs::read(&project.layout().manifest).unwrap();
    let checksum = blake3::hash(LEGACY_CHECKSUM_PAYLOAD.as_bytes()).to_hex();
    let record: JournalRecord = serde_json::from_str(&format!(
        "{},\"checksum\":\"{checksum}\"}}",
        LEGACY_CHECKSUM_PAYLOAD.strip_suffix('}').unwrap()
    ))
    .unwrap();
    record.verify().unwrap();
    assert_eq!(record.command.required_schema_version(), 1);
    journal::append(&project.layout().journal, &record).unwrap();
    drop(project);
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(opened.journal_recovery.replayed_records, 1);
    assert_eq!(opened.project.manifest().schema_version, 1);
    assert_eq!(
        opened.project.manifest().timeline.overlay_tracks[0].frame_cells,
        None
    );
    assert_eq!(fs::read(&opened.project.layout().manifest).unwrap(), before);
}

#[test]
fn recovery_rejects_unstamped_payload_without_allowing_followup_writes() {
    let directory = tempfile::tempdir().unwrap();
    let project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    let record = JournalRecord::new(
        ProjectRevision::ZERO,
        ProjectRevision::new(1),
        new_payload(),
    )
    .unwrap();
    journal::append(&project.layout().journal, &record).unwrap();
    drop(project);
    let mut opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    assert!(matches!(
        opened.journal_recovery.stop_reason,
        Some(JournalStopReason::SchemaMismatch { .. })
    ));
    assert_eq!(opened.project.manifest(), &legacy_manifest());
    assert!(matches!(
        opened.project.commit(legacy_edit()),
        Err(ProjectError::RequiresJournalRepair)
    ));
    assert!(matches!(
        opened.project.stamp_schema_upgrade(2),
        Err(ProjectError::RequiresJournalRepair)
    ));
}

#[test]
fn stamp_and_reopen_reject_unknown_future_formats_without_mutating_legacy_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = ActiveProject::create(directory.path(), legacy_manifest()).unwrap();
    let before = fs::read(&project.layout().manifest).unwrap();
    assert!(project.stamp_schema_upgrade(3).is_err());
    assert!(!project.write_requires_recovery);
    assert_eq!(fs::read(&project.layout().manifest).unwrap(), before);
    let mut future = legacy_manifest();
    future.schema_version = 3;
    write_manifest(&project.layout().manifest, &future).unwrap();
    drop(project);
    assert!(ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).is_err());
}
