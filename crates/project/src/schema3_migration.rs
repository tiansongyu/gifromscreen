//! Schema-3 render programs must never precede their durable manifest gate.

use std::{fs, io, path::Path};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, BlendMode, Canvas, CanvasBackground, CaptureBinding,
    CaptureMetadata, ClipTransform, ColorSpace, DurationUs, EditCommand, FrameClip, FrameId,
    FrameOverlayCell, FrameRenderStep, IndexedFrame, OverlayTrack, PhysicalSize, ProjectId,
    ProjectManifest, ProjectRevision, RasterEncoding, TrackId, UnixTimeMs,
};

use super::{ActiveProject, LockPolicy, ProjectError, read_manifest, write_manifest};
use crate::{JournalRecord, JournalStopReason, journal};

pub(super) fn project(root: &Path, schema: u32) -> ActiveProject {
    let size = PhysicalSize::new(2, 2).unwrap();
    let mut manifest = ProjectManifest::new(
        ProjectId::from_u128(33),
        "schema3-test",
        UnixTimeMs::new(1),
        Canvas {
            size,
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    manifest.schema_version = schema;
    let mut project = ActiveProject::create(root, manifest).unwrap();
    let asset_id = project.assets().put(&[127; 16]).unwrap();
    project
        .commit_recording_append(
            Some(AssetDescriptor {
                id: asset_id,
                byte_len: 16,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            }),
            FrameClip {
                id: FrameId::from_u128(1),
                asset_id,
                duration: DurationUs::new(10_000).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                capture_binding: CaptureBinding::Original,
                capture_clock: None,
                effects: Vec::new(),
                render_steps: Vec::new(),
            },
        )
        .unwrap();
    project
}

fn staged_frame(project: &ActiveProject) -> FrameClip {
    let mut frame = project.manifest().timeline.frames[0].clone();
    frame.render_steps = vec![
        FrameRenderStep::composite(7),
        FrameRenderStep::Resize {
            size: PhysicalSize::new(1, 2).unwrap(),
        },
        FrameRenderStep::composite(9),
    ];
    frame
}

fn anchored_track() -> OverlayTrack {
    let mut cell = FrameOverlayCell::whole(FrameId::from_u128(1), 1, Vec::new());
    cell.stage = Some(7);
    OverlayTrack {
        id: TrackId::from_u128(1),
        frame_cells: Some(vec![cell]),
        annotation: None,
        annotation_scope: None,
        name: "Hidden staged scope".to_owned(),
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    }
}

fn staged_edit(project: &ActiveProject) -> EditCommand {
    EditCommand::Compound {
        commands: vec![
            EditCommand::ReplaceFrame {
                frame_id: FrameId::from_u128(1),
                replacement: Box::new(staged_frame(project)),
            },
            EditCommand::UpsertOverlayTrack {
                track: anchored_track(),
            },
        ],
    }
}

#[test]
fn schema_two_upgrade_stamps_old_committed_frames_before_new_stages_and_undo_is_sticky() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(directory.path(), 2);
    let edit = staged_edit(&project);
    assert_upgrade_roundtrip(directory.path(), project, edit, 3);
}

pub(super) fn assert_upgrade_roundtrip(
    root: &Path,
    mut project: ActiveProject,
    edit: EditCommand,
    required_schema: u32,
) {
    let before = project.manifest().clone();
    let prefix = fs::read(&project.layout().journal).unwrap();
    let receipt = project.commit(edit).unwrap();
    assert_eq!(project.manifest().schema_version, required_schema);
    let mut stamped = before.clone();
    stamped.schema_version = required_schema;
    assert_eq!(read_manifest(&project.layout().manifest).unwrap(), stamped);
    assert!(
        fs::read(&project.layout().journal)
            .unwrap()
            .starts_with(&prefix)
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
    assert_eq!(project.manifest().schema_version, required_schema);
    assert_eq!(project.manifest().timeline, before.timeline);
    assert_eq!(project.manifest().assets, before.assets);
    project.checkpoint_and_compact().unwrap();
    drop(project);
    let mut opened = ActiveProject::open(root, LockPolicy::FailIfPresent)
        .unwrap()
        .project;
    assert_eq!(opened.manifest().schema_version, required_schema);
    opened.commit(redo).unwrap();
    assert_eq!(opened.manifest().timeline, after.timeline);
    assert_eq!(opened.manifest().assets, after.assets);
    assert_eq!(
        opened
            .assets()
            .read(after.timeline.frames[0].asset_id)
            .unwrap(),
        [127; 16]
    );
}

#[test]
fn invalid_stage_payload_never_upgrades_a_legacy_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 2);
    let before = project.manifest().clone();
    let disk = fs::read(&project.layout().manifest).unwrap();
    let journal = fs::read(&project.layout().journal).unwrap();
    let mut frame = staged_frame(&project);
    frame.render_steps.push(FrameRenderStep::composite(7));
    assert!(
        project
            .commit(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            })
            .is_err()
    );
    assert_eq!(project.manifest(), &before);
    assert_eq!(fs::read(&project.layout().manifest).unwrap(), disk);
    assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
}

#[test]
fn raw_recording_fast_path_rejects_render_steps_in_every_supported_schema() {
    for schema in [1, 2, 3, 4, 5] {
        let directory = tempfile::tempdir().unwrap();
        let mut project = project(directory.path(), schema);
        let before = project.manifest().clone();
        let journal = fs::read(&project.layout().journal).unwrap();
        let mut frame = staged_frame(&project);
        frame.id = FrameId::from_u128(2);
        assert!(matches!(
            project.commit_recording_append(None, frame),
            Err(ProjectError::InvalidRecordingMutation(_))
        ));
        assert_eq!(project.manifest(), &before);
        assert_eq!(fs::read(&project.layout().journal).unwrap(), journal);
    }
}

#[test]
fn schema_two_journal_refuses_new_frame_and_anchor_payloads_before_replay() {
    let directory = tempfile::tempdir().unwrap();
    let project = project(&directory.path().join("source"), 2);
    let frame = staged_frame(&project);
    let track = anchored_track();
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
            replacement: Box::new(frame),
        },
        EditCommand::UpsertOverlayTrack {
            track: track.clone(),
        },
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: track.clone(),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::RemoveFrames {
                frame_ids: vec![FrameId::from_u128(1)],
            }),
            overlay_tracks: vec![track],
        },
    ];
    for (index, command) in commands.into_iter().enumerate() {
        let path = directory.path().join(format!("unstamped-{index}.ndjson"));
        let command = EditCommand::Compound {
            commands: vec![command],
        };
        let record =
            JournalRecord::new(ProjectRevision::new(1), ProjectRevision::new(2), command).unwrap();
        journal::append(&path, &record).unwrap();
        let before = project.manifest().clone();
        let recovered = journal::recover(before.clone(), &path).unwrap();
        assert_eq!(recovered.manifest, before);
        assert_eq!(
            recovered.report.stop_reason,
            Some(JournalStopReason::SchemaMismatch {
                line: 1,
                snapshot_schema: 2,
                required_schema: 3,
            })
        );
    }
}

#[test]
fn ambiguous_v3_stamp_blocks_further_writes_and_recovers_only_the_old_visual_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = project(directory.path(), 2);
    let before = project.manifest().clone();
    let journal = fs::read(&project.layout().journal).unwrap();
    let error = project
        .stamp_schema_upgrade_with(3, |path, manifest| {
            write_manifest(path, manifest)?;
            Err(ProjectError::io(
                "injected post-publication schema3 failure",
                path,
                io::Error::other("injected"),
            ))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectError::SchemaUpgradeFailed {
            from_version: 2,
            to_version: 3,
            ..
        }
    ));
    assert_eq!(project.manifest(), &before);
    assert!(matches!(
        project.commit(staged_edit(&project)),
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
    expected.schema_version = 3;
    assert_eq!(opened.project.manifest(), &expected);
}
