use super::{tests::project, *};
use gif_from_screen_domain::{AssetDescriptor, AssetId, AssetKind, FrameRenderStep, PhysicalSize};

#[test]
fn copied_snapshot_refs_survive_source_frame_deletion_and_are_preflighted_before_new_ids() {
    let mut project = project(3);
    let id = AssetId::from_digest([91; 32]);
    let size = PhysicalSize::new(2, 2).unwrap();
    let descriptor = AssetDescriptor {
        id,
        byte_len: 33,
        kind: AssetKind::PremultipliedSnapshot {
            size,
            format_version: 1,
        },
    };
    project.assets.insert(id, descriptor.clone());
    project.timeline.frames[0].render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::CinemagraphOverlay {
            snapshot_asset: id,
            snapshot_size: size,
        },
    ];
    let original = project.timeline.frames[0].clone();
    let clipboard = copy_selected_frames(&project, [original.id]).unwrap();
    project
        .apply_command(&crate::remove_frames_atomically(
            &project,
            vec![original.id],
        ))
        .unwrap();
    for replacement in [
        None,
        Some(AssetDescriptor {
            kind: AssetKind::PremultipliedSnapshot {
                size: PhysicalSize::new(1, 4).unwrap(),
                format_version: 1,
            },
            ..descriptor.clone()
        }),
        Some(AssetDescriptor {
            kind: AssetKind::ImportedSource {
                media_type: "application/octet-stream".into(),
            },
            ..descriptor.clone()
        }),
    ] {
        project.assets.remove(&id);
        if let Some(asset) = replacement {
            project.assets.insert(id, asset);
        }
        let before = project.clone();
        let mut generated = 0;
        assert!(
            paste_frame_clipboard(&project, &clipboard, None, || {
                generated += 1;
                FrameId::from_u128(100)
            })
            .is_err()
        );
        assert_eq!(generated, 0);
        assert_eq!(project, before);
    }
    project.assets.insert(id, descriptor);
    let pasted = FrameId::from_u128(100);
    let command = paste_frame_clipboard(&project, &clipboard, None, || pasted).unwrap();
    project.apply_command(&command).unwrap();
    let frame = project
        .timeline
        .frames
        .iter()
        .find(|frame| frame.id == pasted)
        .unwrap();
    assert_eq!(frame.render_steps, original.render_steps);
    assert_eq!(
        frame.referenced_effect_assets().collect::<Vec<_>>(),
        vec![id]
    );
    project.validate().unwrap();
}
