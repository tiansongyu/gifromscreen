//! Actual frame geometry, not a stale document header, gates insertion.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use gif_from_screen_domain::{
    AssetKind, Canvas, CanvasBackground, CaptureBinding, CaptureMetadata, ClipTransform,
    ColorSpace, DurationUs, Effect, FrameGeometryPlan, PhysicalRect, PhysicalSize, ProjectId,
    UnixTimeMs,
};
use gif_from_screen_gif::NeverCancel;

fn size(side: u32) -> PhysicalSize {
    PhysicalSize::new(side, side).unwrap()
}

fn workspace(root: &Path, count: usize, side: u32, color: [u8; 4]) -> EditorWorkspace {
    let mut manifest = ProjectManifest::new(
        ProjectId::from_u128(Uuid::new_v4().as_u128()),
        "insertion-geometry-test",
        UnixTimeMs::new(0),
        Canvas {
            size: size(side),
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    manifest.schema_version = 2;
    let mut project = ActiveProject::create(root, manifest).unwrap();
    let pixels = color.repeat(usize::try_from(size(side).area().unwrap()).unwrap());
    let asset_id = project.assets().put(&pixels).unwrap();
    project
        .commit(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: AssetDescriptor {
                        id: asset_id,
                        byte_len: pixels.len() as u64,
                        kind: AssetKind::Frame {
                            size: size(side),
                            encoding: RasterEncoding::Rgba8,
                        },
                    },
                },
                EditCommand::InsertFrames {
                    index: 0,
                    frames: (0..count)
                        .map(|index| FrameClip {
                            id: FrameId::from_u128(index as u128 + 1),
                            asset_id,
                            duration: DurationUs::new(10_000).unwrap(),
                            transform: ClipTransform::default(),
                            capture_metadata: CaptureMetadata::default(),
                            capture_binding: CaptureBinding::NotRecorded,
                            capture_clock: None,
                            effects: Vec::new(),
                            render_steps: Vec::new(),
                        })
                        .collect(),
                },
            ],
        })
        .unwrap();
    EditorWorkspace::from_active(project, 16).unwrap()
}

fn crop_legacy(workspace: &mut EditorWorkspace, count: usize) {
    let commands = workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .take(count)
        .map(|frame| {
            let mut replacement = frame.clone();
            replacement.transform.crop = Some(PhysicalRect::new(0, 0, 20, 20).unwrap());
            EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(replacement),
            }
        })
        .collect();
    workspace
        .execute(EditCommand::Compound { commands })
        .unwrap();
}

fn corrupt_fixture_pixels(workspace: &EditorWorkspace) {
    for asset in workspace.manifest().assets.values() {
        fs::write(workspace.active_project().assets().asset_path(asset.id), []).unwrap();
    }
}

fn assert_preflight_rejection(
    destination: &EditorWorkspace,
    source: &EditorWorkspace,
    expected: impl FnOnce(&ProjectInsertionError) -> bool,
) {
    let before = destination.manifest().clone();
    let journal = fs::read(&destination.active_project().layout().journal).unwrap();
    let blobs = fs::read_dir(destination.active_project().assets().directory())
        .unwrap()
        .count();
    let result = prepare_project_insertion(
        destination.project_insertion_target(None).unwrap(),
        source.manifest(),
        source.active_project().assets(),
        &NeverCancel,
    )
    .unwrap_err();
    assert!(expected(&result), "{result}");
    assert_eq!(destination.manifest(), &before);
    assert_eq!(
        fs::read(&destination.active_project().layout().journal).unwrap(),
        journal
    );
    assert_eq!(
        fs::read_dir(destination.active_project().assets().directory())
            .unwrap()
            .count(),
        blobs
    );
}

#[test]
fn staged_source_twenty_and_destination_hundred_are_rejected_before_pixel_io() {
    let root = tempfile::tempdir().unwrap();
    let mut source = workspace(&root.path().join("source"), 1, 100, [0, 0, 255, 255]);
    let destination = workspace(&root.path().join("destination"), 1, 100, [255, 0, 0, 255]);
    crop_legacy(&mut source, 1);
    source.select_first().unwrap();
    source.toggle_selection_horizontal_flip().unwrap();
    assert_eq!(source.manifest().canvas.size, size(100));
    assert!(!source.manifest().timeline.frames[0].render_steps.is_empty());
    // If pixel I/O preceded geometry validation, this would fail as damaged
    // raster data instead of reporting the actual-size conflict.
    corrupt_fixture_pixels(&source);
    assert_preflight_rejection(&destination, &source, |error| {
        matches!(error,
        ProjectInsertionError::RenderedSizeMismatch { side: "destination", expected, actual, .. }
            if *expected == size(20) && *actual == size(100))
    });
}

#[test]
fn every_source_and_destination_frame_is_checked_not_just_the_first() {
    for mixed_source in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut source = workspace(&root.path().join("source"), 2, 100, [0, 0, 255, 255]);
        let mut destination = workspace(&root.path().join("destination"), 2, 100, [255, 0, 0, 255]);
        crop_legacy(&mut source, if mixed_source { 1 } else { 2 });
        crop_legacy(&mut destination, if mixed_source { 2 } else { 1 });
        corrupt_fixture_pixels(&source);
        assert_preflight_rejection(&destination, &source, |error| {
            matches!(error,
            ProjectInsertionError::RenderedSizeMismatch { side, frame_id, expected, actual }
                if *side == if mixed_source { "source" } else { "destination" }
                    && *frame_id == FrameId::from_u128(2) && *expected == size(20) && *actual == size(100))
        });
    }
}

fn assert_normalized_export(workspace: &EditorWorkspace) {
    let output = workspace.project_root().join("normalized.gif");
    gif_from_screen_application::export_project_snapshot_to_gif(
        &gif_from_screen_application::ProjectExportSnapshot::from_active(
            workspace.active_project(),
        ),
        &output,
        &gif_from_screen_application::ProjectGifExportOptions::default(),
        &NeverCancel,
        &mut gif_from_screen_application::NoopProjectExportProgress,
    )
    .unwrap();
    let animation = gif_from_screen_media::decode_gif(
        File::open(output).unwrap(),
        &gif_from_screen_media::GifDecodeOptions::default(),
    )
    .unwrap();
    assert_eq!((animation.width(), animation.height()), (20, 20));
    assert!(
        animation
            .frames()
            .iter()
            .all(|frame| frame.rgba().len() == 20 * 20 * 4)
    );
    assert_eq!(
        animation
            .frames()
            .iter()
            .map(gif_from_screen_media::DecodedFrame::duration_us)
            .sum::<u64>(),
        40_000
    );
}

#[test]
fn matching_legacy_outputs_allow_stale_different_headers_and_normalize_atomically() {
    for destination_header in [100, 80] {
        let root = tempfile::tempdir().unwrap();
        let mut source = workspace(&root.path().join("source"), 2, 100, [0, 0, 255, 255]);
        let destination_root = root.path().join("destination");
        let mut destination = workspace(&destination_root, 2, destination_header, [255, 0, 0, 255]);
        crop_legacy(&mut source, 2);
        crop_legacy(&mut destination, 2);
        let before = destination.manifest().clone();
        let source_before = source.manifest().clone();
        let pixels = crate::editor_preview::render_frame_surface(
            destination.active_project(),
            FrameId::from_u128(1),
            1024 * 1024,
        )
        .unwrap();
        let prepared = prepare_project_insertion(
            destination
                .project_insertion_target(Some(FrameId::from_u128(1)))
                .unwrap(),
            source.manifest(),
            source.active_project().assets(),
            &NeverCancel,
        )
        .unwrap();
        destination.insert_prepared_project(prepared).unwrap();
        assert_eq!(destination.manifest().canvas.size, size(20));
        assert_eq!(
            destination.manifest().revision.get(),
            before.revision.get() + 1
        );
        assert_eq!(destination.manifest().timeline.frames.len(), 4);
        assert_eq!(source.manifest(), &source_before);
        let after_pixels = crate::editor_preview::render_frame_surface(
            destination.active_project(),
            FrameId::from_u128(1),
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(after_pixels, pixels);
        let after = destination.manifest().clone();
        destination.undo().unwrap();
        super::tests::same_content(destination.manifest(), &before);
        destination.redo().unwrap();
        super::tests::same_content(destination.manifest(), &after);
        drop(destination);
        let reopened =
            EditorWorkspace::open(&destination_root, LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().canvas.size, size(20));
        assert_normalized_export(&reopened);
    }
}

#[test]
fn empty_destination_uses_source_output_and_undo_restores_its_old_header() {
    let root = tempfile::tempdir().unwrap();
    let mut source = workspace(&root.path().join("source"), 1, 100, [0, 0, 255, 255]);
    let mut destination = workspace(&root.path().join("destination"), 0, 80, [255, 0, 0, 255]);
    crop_legacy(&mut source, 1);
    let before = destination.manifest().clone();
    let prepared = prepare_project_insertion(
        destination.project_insertion_target(None).unwrap(),
        source.manifest(),
        source.active_project().assets(),
        &NeverCancel,
    )
    .unwrap();
    destination.insert_prepared_project(prepared).unwrap();
    assert_eq!(destination.manifest().canvas.size, size(20));
    let frame = &destination.manifest().timeline.frames[0];
    assert_eq!(
        FrameGeometryPlan::new(frame, size(100))
            .unwrap()
            .output_size(),
        size(20)
    );
    destination.undo().unwrap();
    super::tests::same_content(destination.manifest(), &before);
}

#[test]
fn unrenderable_legacy_effects_are_rejected_before_pixel_or_destination_io() {
    for cinemagraph in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut source = workspace(&root.path().join("source"), 1, 100, [0, 0, 255, 255]);
        let destination = workspace(&root.path().join("destination"), 1, 100, [255, 0, 0, 255]);
        crop_legacy(&mut source, 1);
        let mut frame = source.manifest().timeline.frames[0].clone();
        frame.effects.push(if cinemagraph {
            Effect::Cinemagraph {
                mask_asset: frame.asset_id,
                invert_mask: false,
            }
        } else {
            Effect::Blur {
                region: PhysicalRect::new(80, 80, 10, 10).unwrap(),
                radius: 1,
            }
        });
        source
            .execute(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            })
            .unwrap();
        source.manifest().validate().unwrap();
        corrupt_fixture_pixels(&source);
        assert_preflight_rejection(&destination, &source, |error| {
            matches!(error,
            ProjectInsertionError::RenderedGeometry { side: "source", reason, .. } if reason.contains("Legacy effect 1"))
        });
    }
}

struct CancelAfter {
    checks: AtomicUsize,
    allowed: usize,
}
impl CancellationToken for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) >= self.allowed
    }
}

#[test]
fn geometry_preflight_observes_cancellation_between_both_source_and_destination_frames() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(&root.path().join("source"), 2, 100, [0, 0, 255, 255]);
    let destination = workspace(&root.path().join("destination"), 3, 100, [255, 0, 0, 255]);
    for allowed in [1, 3] {
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            allowed,
        };
        assert!(matches!(
            validate_rendered_sizes(source.manifest(), destination.manifest(), &cancellation),
            Err(ProjectInsertionError::Cancelled)
        ));
        assert_eq!(cancellation.checks.load(Ordering::Relaxed), allowed + 1);
    }
}
