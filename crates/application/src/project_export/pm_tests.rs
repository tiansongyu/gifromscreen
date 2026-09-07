use super::{
    tests::{TestClip, add_transition, decode_rgba, export, snapshot},
    *,
};
use gif_from_screen_domain::{AssetDescriptor, Effect, PhysicalSize, TransitionKind};
use gif_from_screen_gif::{NeverCancel, PaletteMode};

fn pm_asset(snapshot: &mut ProjectExportSnapshot, size: PhysicalSize, bytes: &[u8]) -> AssetId {
    let id = snapshot.assets.put(bytes).unwrap();
    snapshot.manifest.assets.insert(
        id,
        AssetDescriptor {
            id,
            byte_len: u64::try_from(bytes.len()).unwrap(),
            kind: AssetKind::PremultipliedSnapshot {
                size,
                format_version: 1,
            },
        },
    );
    id
}

fn attach(snapshot: &mut ProjectExportSnapshot, id: AssetId, count: usize) {
    for frame in &mut snapshot.manifest.timeline.frames {
        frame.render_steps = vec![FrameRenderStep::composite(1)];
        frame.render_steps.extend(std::iter::repeat_n(
            FrameRenderStep::CinemagraphOverlay {
                snapshot_asset: id,
                snapshot_size: PhysicalSize::new(2, 1).unwrap(),
            },
            count,
        ));
    }
    snapshot.manifest.schema_version = 7;
}

fn fixture(count: usize) -> (tempfile::TempDir, ProjectExportSnapshot, AssetId) {
    let dir = tempfile::tempdir().unwrap();
    let size = PhysicalSize::new(2, 1).unwrap();
    let (mut snapshot, _) = snapshot(
        dir.path(),
        size,
        &[
            TestClip::rgba(1, &[255, 0, 0, 255].repeat(2), 100_000),
            TestClip::rgba(2, &[0, 0, 255, 255].repeat(2), 100_000),
        ],
    );
    let bytes = PremultipliedRgbaSurface::new(size, vec![0, 0, 0, 0, 0, 128, 0, 128])
        .unwrap()
        .encode(25)
        .unwrap();
    let id = pm_asset(&mut snapshot, size, &bytes);
    attach(&mut snapshot, id, count);
    (dir, snapshot, id)
}

#[test]
fn typed_export_provider_has_one_aggregate_budget_and_no_straight_pm_alias() {
    let (_dir, snapshot, id) = fixture(2);
    let clips = &snapshot.manifest.timeline.frames;
    let times = selected_frame_start_times(&snapshot.manifest, clips).unwrap();
    assert!(matches!(
        load_selected_assets(&snapshot, clips, &times, 40, &NeverCancel),
        Err(ProjectGifExportError::RenderBufferLimitExceeded {
            required_bytes: 41,
            ..
        })
    ));
    let (provider, retained) =
        load_selected_assets(&snapshot, clips, &times, 41, &NeverCancel).unwrap();
    assert_eq!(retained, 41); // two 8-byte originals plus one 25-byte encoded PM allocation
    assert_eq!(provider.assets.len(), 2);
    assert_eq!(provider.premultiplied.len(), 1);
    assert!(provider.load_rgba8(id).is_err());
    assert_eq!(
        provider.load_premultiplied_rgba8(id).unwrap().pixels(),
        &[0, 0, 0, 0, 0, 128, 0, 128]
    );
    assert!(
        provider
            .load_premultiplied_rgba8(clips[0].asset_id)
            .is_err()
    );
}

#[test]
fn typed_snapshots_feed_local_and_global_gif_transition_endpoints() {
    let (dir, mut snapshot, _) = fixture(1);
    add_transition(&mut snapshot, 1, 2, 20_000, 1, TransitionKind::FadeToNext);
    let colors = [
        [255, 0, 0],
        [127, 128, 0],
        [0, 0, 255],
        [0, 128, 127],
        [128, 0, 128],
        [64, 128, 64],
    ]
    .concat();
    let expected = vec![
        [255, 0, 0, 255, 127, 128, 0, 255].to_vec(),
        [128, 0, 128, 255, 64, 128, 64, 255].to_vec(),
        [0, 0, 255, 255, 0, 128, 127, 255].to_vec(),
    ];
    for (index, mode) in [PaletteMode::LocalPerFrame, PaletteMode::Global]
        .into_iter()
        .enumerate()
    {
        let mut options = ProjectGifExportOptions {
            custom_palette: Some(CustomGifPalette::new(colors.clone(), None).unwrap()),
            render_buffer_limit_bytes: 1024,
            ..ProjectGifExportOptions::default()
        };
        options.encoding.palette_mode = mode;
        let output = dir.path().join(format!("typed-{index}.gif"));
        export(&snapshot, &output, &options).unwrap();
        assert_eq!(
            decode_rgba(&output)
                .into_iter()
                .map(|(_, pixels)| pixels)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn typed_export_load_rejects_missing_corrupt_length_and_forged_pixels() {
    for mode in 0..4 {
        let (dir, mut snapshot, original) = fixture(1);
        let mut id = original;
        let path = snapshot.assets.asset_path(id);
        match mode {
            0 => fs::remove_file(path).unwrap(),
            1 => fs::write(path, [0_u8; 25]).unwrap(),
            2 => fs::write(path, [0_u8; 24]).unwrap(),
            _ => {
                let mut bytes = snapshot.assets.read(id).unwrap();
                bytes[17] = 1;
                id = pm_asset(&mut snapshot, PhysicalSize::new(2, 1).unwrap(), &bytes);
                attach(&mut snapshot, id, 1);
            }
        }
        let clips = &snapshot.manifest.timeline.frames;
        let times = selected_frame_start_times(&snapshot.manifest, clips).unwrap();
        let error = load_selected_assets(&snapshot, clips, &times, 1024, &NeverCancel).unwrap_err();
        assert!(match error {
            ProjectGifExportError::MissingAssetFile { asset_id, .. } => mode == 0 && asset_id == id,
            ProjectGifExportError::CorruptAsset { asset_id, .. } => mode == 1 && asset_id == id,
            ProjectGifExportError::AssetLengthMismatch { asset_id, .. } =>
                mode == 2 && asset_id == id,
            ProjectGifExportError::InvalidPremultipliedSnapshot {
                asset_id,
                source: PremultipliedSnapshotError::InvalidPixel { .. },
                ..
            } => mode == 3 && asset_id == id,
            other => panic!("unexpected typed error {other}"),
        });
        let target = dir.path().join("unchanged.gif");
        fs::write(&target, b"original output").unwrap();
        let options = ProjectGifExportOptions {
            overwrite_existing: true,
            ..ProjectGifExportOptions::default()
        };
        assert!(export(&snapshot, &target, &options).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original output");
    }
}

#[test]
fn typed_descriptor_and_shape_misuse_never_fall_back_to_raw_rgba() {
    let (_dir, mut snapshot, id) = fixture(1);
    let times = vec![TimeUs::ZERO; 2];
    let descriptor = snapshot.manifest.assets.remove(&id).unwrap();
    assert!(
        matches!(load_selected_assets(&snapshot, &snapshot.manifest.timeline.frames, &times, 1024, &NeverCancel), Err(ProjectGifExportError::MissingAssetDescriptor { asset_id, .. }) if asset_id == id)
    );
    snapshot.manifest.assets.insert(id, descriptor);
    let FrameRenderStep::CinemagraphOverlay { snapshot_size, .. } =
        &mut snapshot.manifest.timeline.frames[0].render_steps[1]
    else {
        panic!("snapshot");
    };
    *snapshot_size = PhysicalSize::new(1, 2).unwrap();
    assert!(matches!(
        load_selected_assets(
            &snapshot,
            &snapshot.manifest.timeline.frames,
            &times,
            1024,
            &NeverCancel
        ),
        Err(ProjectGifExportError::InvalidPremultipliedDescriptor { .. })
    ));
    snapshot.manifest.timeline.frames[0].asset_id = id;
    assert!(
        matches!(load_selected_assets(&snapshot, &snapshot.manifest.timeline.frames, &times, 1024, &NeverCancel), Err(ProjectGifExportError::InvalidAssetKind { asset_id, .. }) if asset_id == id)
    );
}

#[test]
fn legacy_cinemagraph_mask_remains_unsupported_without_loading_its_unknown_asset() {
    let (_dir, mut snapshot, _) = fixture(1);
    let clip = &mut snapshot.manifest.timeline.frames[0];
    clip.render_steps.clear();
    clip.effects = vec![Effect::Cinemagraph {
        mask_asset: AssetId::from_digest([99; 32]),
        invert_mask: false,
    }];
    let clips = &snapshot.manifest.timeline.frames[..1];
    let (provider, _) =
        load_selected_assets(&snapshot, clips, &[TimeUs::ZERO], 1024, &NeverCancel).unwrap();
    assert!(provider.premultiplied.is_empty());
    let error = CpuRenderer::new()
        .render_clip(&clips[0], &provider, &gif_from_screen_render::NeverCancel)
        .unwrap_err();
    assert!(matches!(
        error,
        RenderError::UnsupportedEffect(gif_from_screen_render::UnsupportedEffect::Cinemagraph)
    ));
}
