use super::{
    tests::{project_with_frame, transition_project},
    *,
};
use gif_from_screen_domain::{ClipTransform, EditCommand, PhysicalSize, TransitionKind};

fn pm_asset(project: &mut ActiveProject, size: PhysicalSize, bytes: &[u8]) -> AssetId {
    let id = project.assets().put(bytes).unwrap();
    project
        .commit(EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: u64::try_from(bytes.len()).unwrap(),
                kind: AssetKind::PremultipliedSnapshot {
                    size,
                    format_version: 1,
                },
            },
        })
        .unwrap();
    id
}

fn attach(
    project: &mut ActiveProject,
    id: FrameId,
    asset: AssetId,
    size: PhysicalSize,
    count: usize,
) {
    let mut frame = project
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|frame| frame.id == id)
        .unwrap()
        .clone();
    frame.render_steps = vec![FrameRenderStep::composite(1)];
    frame.render_steps.extend(std::iter::repeat_n(
        FrameRenderStep::CinemagraphOverlay {
            snapshot_asset: asset,
            snapshot_size: size,
        },
        count,
    ));
    project
        .commit(EditCommand::ReplaceFrame {
            frame_id: id,
            replacement: Box::new(frame),
        })
        .unwrap();
}

fn fixture(count: usize) -> (tempfile::TempDir, ActiveProject, FrameId, AssetId) {
    let size = PhysicalSize::new(2, 1).unwrap();
    let (dir, mut project, frame, _) = project_with_frame(
        &[255, 0, 0, 255].repeat(2),
        size,
        ClipTransform::default(),
        Vec::new(),
    );
    let bytes = PremultipliedRgbaSurface::new(size, vec![0, 0, 0, 0, 0, 128, 0, 128])
        .unwrap()
        .encode(25)
        .unwrap();
    let snapshot = pm_asset(&mut project, size, &bytes);
    attach(&mut project, frame, snapshot, size, count);
    (dir, project, frame, snapshot)
}

#[test]
fn typed_snapshot_preview_deduplicates_and_counts_encoded_capacity_in_one_budget() {
    let (_dir, project, frame, snapshot) = fixture(2);
    let clip = &project.manifest().timeline.frames[0];
    let plan = PreviewRenderPlan::new(&project, clip, TimeUs::ZERO).unwrap();
    assert_eq!(plan.descriptors.len(), 2);
    assert!(
        matches!(plan.render(32, &NeverCancel), Err(EditorPreviewError::SourceMemoryLimitExceeded { asset_id, required: 33, .. }) if asset_id == snapshot)
    );
    let output = plan.render(33, &NeverCancel).unwrap();
    assert_eq!(output.pixels(), &[255, 0, 0, 255, 63, 192, 0, 255]);
    assert_eq!(render_frame_surface(&project, frame, 33).unwrap(), output);
    let mut typed = BTreeMap::new();
    let mut retained = 0;
    load_preview_premultiplied(
        &plan.store,
        &plan.descriptors,
        frame,
        snapshot,
        output.size(),
        25,
        &mut retained,
        &mut typed,
    )
    .unwrap();
    assert_eq!(retained, 25);
    let provider = PreviewAssetProvider {
        assets: BTreeMap::new(),
        premultiplied: typed,
    };
    assert!(provider.load_rgba8(snapshot).is_err());
    assert_eq!(
        provider
            .load_premultiplied_rgba8(snapshot)
            .unwrap()
            .pixels(),
        &[0, 0, 0, 0, 0, 128, 0, 128]
    );
}

#[test]
fn typed_snapshot_preview_rejects_missing_corrupt_short_and_forged_containers() {
    for mode in 0..4 {
        let (_dir, mut project, frame, original) = fixture(1);
        let mut snapshot = original;
        let path = project.assets().asset_path(snapshot);
        match mode {
            0 => fs::remove_file(path).unwrap(),
            1 => fs::write(path, [0_u8; 25]).unwrap(),
            2 => fs::write(path, [0_u8; 24]).unwrap(),
            _ => {
                let mut bytes = project.assets().read(snapshot).unwrap();
                bytes[17] = 1; // RGB beneath alpha zero, with its own matching digest.
                snapshot = pm_asset(&mut project, PhysicalSize::new(2, 1).unwrap(), &bytes);
                attach(
                    &mut project,
                    frame,
                    snapshot,
                    PhysicalSize::new(2, 1).unwrap(),
                    1,
                );
            }
        }
        let error = render_frame_surface(&project, frame, 1024).unwrap_err();
        assert!(match error {
            EditorPreviewError::AssetMetadata { asset_id, .. } => mode == 0 && asset_id == snapshot,
            EditorPreviewError::AssetRead {
                asset_id,
                source: ProjectError::CorruptAsset { .. },
            } => mode == 1 && asset_id == snapshot,
            EditorPreviewError::AssetFileLengthMismatch { asset_id, .. } =>
                mode == 2 && asset_id == snapshot,
            EditorPreviewError::InvalidPremultipliedSnapshot {
                asset_id,
                source: PremultipliedSnapshotError::InvalidPixel { .. },
            } => mode == 3 && asset_id == snapshot,
            other => panic!("unexpected typed load error: {other}"),
        });
    }
}

#[test]
fn typed_snapshot_plan_rejects_descriptor_removal_raw_misuse_and_same_area_shape_alias() {
    let (_dir, project, frame, snapshot) = fixture(1);
    let mut plan = PreviewRenderPlan::new(
        &project,
        &project.manifest().timeline.frames[0],
        TimeUs::ZERO,
    )
    .unwrap();
    let descriptor = plan.descriptors.remove(&snapshot).unwrap();
    assert!(
        matches!(plan.render(1024, &NeverCancel), Err(EditorPreviewError::MissingAssetDescriptor { asset_id, .. }) if asset_id == snapshot)
    );
    plan.descriptors.insert(snapshot, descriptor);
    let FrameRenderStep::CinemagraphOverlay { snapshot_size, .. } = &mut plan.clip.render_steps[1]
    else {
        panic!("snapshot");
    };
    *snapshot_size = PhysicalSize::new(1, 2).unwrap();
    assert!(matches!(
        plan.render(1024, &NeverCancel),
        Err(EditorPreviewError::InvalidPremultipliedDescriptor { .. })
    ));
    plan.clip.asset_id = snapshot;
    assert!(
        matches!(plan.render(1024, &NeverCancel), Err(EditorPreviewError::InvalidAssetKind { asset_id, .. }) if asset_id == snapshot)
    );
    assert_eq!(plan.clip.id, frame);
}

#[test]
fn transition_preview_loads_typed_snapshots_for_both_original_endpoints() {
    let (_dir, mut project, step) = transition_project(TransitionKind::FadeToNext);
    let size = PhysicalSize::new(2, 1).unwrap();
    let bytes = PremultipliedRgbaSurface::new(size, vec![0, 0, 0, 0, 0, 128, 0, 128])
        .unwrap()
        .encode(25)
        .unwrap();
    let snapshot = pm_asset(&mut project, size, &bytes);
    attach(&mut project, step.from_frame, snapshot, size, 1);
    attach(&mut project, step.to_frame, snapshot, size, 1);
    let actual = render_transition_surface(&project, step, 111).unwrap();
    // The existing fixture's outgoing legacy raster covers pixel 1 in green.
    assert_eq!(actual.pixels(), &[128, 0, 128, 255, 0, 192, 64, 255]);
    assert!(matches!(
        render_transition_surface(&project, step, 110),
        Err(EditorPreviewError::SourceMemoryLimitExceeded { required: 37, .. })
    ));
}
