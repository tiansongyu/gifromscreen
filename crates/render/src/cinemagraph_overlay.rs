//! One premultiplied reference draw, followed by exactly one PNG/WIC boundary.

use super::check_cancelled;
use crate::{
    CancellationToken, FrameAssetProvider, RenderError, RenderLimits, RgbaSurface,
    surface::checked_byte_len, wpf_pixels,
};
use gif_from_screen_domain::{AssetId, PhysicalSize};

pub(super) fn apply<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    snapshot_asset: AssetId,
    snapshot_size: PhysicalSize,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancellation)?;
    if destination.size() != snapshot_size {
        return Err(RenderError::CinemagraphSnapshotSizeMismatch {
            asset_id: snapshot_asset,
            expected: destination.size(),
            actual: snapshot_size,
        });
    }
    let bytes = checked_byte_len(snapshot_size)?;
    let requested = bytes
        .checked_mul(2)
        .ok_or(RenderError::EffectWorkingMemorySizeOverflow {
            effect: "cinemagraph",
        })?;
    if requested > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "cinemagraph",
            requested,
            limit: limits.max_surface_bytes,
        });
    }
    let snapshot = provider
        .load_premultiplied_rgba8(snapshot_asset)
        .map_err(|source| RenderError::CinemagraphSnapshotLoad {
            asset_id: snapshot_asset,
            source,
        })?;
    check_cancelled(cancellation)?;
    if snapshot.size() != snapshot_size {
        return Err(RenderError::CinemagraphSnapshotSizeMismatch {
            asset_id: snapshot_asset,
            expected: snapshot_size,
            actual: snapshot.size(),
        });
    }
    for (index, (pixel, reference)) in destination
        .pixels_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(snapshot.pixels().as_chunks::<4>().0)
        .enumerate()
    {
        if index.is_multiple_of(1024) {
            check_cancelled(cancellation)?;
        }
        *pixel = wpf_pixels::unpremultiply(wpf_pixels::over(
            *reference,
            wpf_pixels::premultiply(*pixel),
        ));
    }
    check_cancelled(cancellation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetProviderError, CpuRenderer, NeverCancel, OverlayRenderPlan, PremultipliedRgbaSurface,
    };
    use gif_from_screen_domain::{
        CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, FrameClip, FrameId,
        FrameRenderStep, TimeUs,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn asset(number: u8) -> AssetId {
        AssetId::from_digest([number; 32])
    }
    fn size(width: u32, height: u32) -> PhysicalSize {
        PhysicalSize::new(width, height).unwrap()
    }
    fn frame(size: PhysicalSize) -> FrameClip {
        FrameClip {
            id: FrameId::from_u128(1),
            asset_id: asset(1),
            duration: DurationUs::new(10).unwrap(),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            capture_metadata: CaptureMetadata::default(),
            capture_binding: CaptureBinding::Original,
            capture_clock: None,
            render_steps: vec![
                FrameRenderStep::composite(1),
                FrameRenderStep::CinemagraphOverlay {
                    snapshot_asset: asset(2),
                    snapshot_size: size,
                },
            ],
        }
    }
    struct Assets {
        source: RgbaSurface,
        snapshot: PremultipliedRgbaSurface,
        loads: AtomicUsize,
    }
    impl FrameAssetProvider for Assets {
        fn load_rgba8(&self, id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
            assert_eq!(
                id,
                asset(1),
                "PM references cannot go through the straight loader"
            );
            Ok(self.source.clone())
        }
        fn load_premultiplied_rgba8(
            &self,
            id: AssetId,
        ) -> Result<PremultipliedRgbaSurface, AssetProviderError> {
            assert_eq!(id, asset(2));
            self.loads.fetch_add(1, Ordering::Relaxed);
            Ok(self.snapshot.clone())
        }
    }

    #[test]
    fn snapshot_stays_premultiplied_until_one_final_wic_boundary_on_every_render_route() {
        let assets = Assets {
            source: RgbaSurface::new(
                size(3, 1),
                vec![0, 0, 0, 0, 90, 20, 130, 64, 255, 255, 255, 255],
            )
            .unwrap(),
            snapshot: PremultipliedRgbaSurface::new(
                size(3, 1),
                vec![0, 0, 253, 253, 0, 0, 0, 0, 0, 32, 64, 128],
            )
            .unwrap(),
            loads: AtomicUsize::new(0),
        };
        let frame = frame(size(3, 1));
        let renderer = CpuRenderer::default();
        let direct = renderer.render_clip(&frame, &assets, &NeverCancel).unwrap();
        assert_eq!(
            direct.pixels(),
            &[0, 0, 254, 253, 91, 19, 131, 64, 127, 159, 191, 255]
        );
        assert_eq!(
            renderer
                .render_clip_with_overlays(&frame, &[], TimeUs::ZERO, &assets, &NeverCancel)
                .unwrap(),
            direct
        );
        let plan = OverlayRenderPlan::for_frame(&[], frame.id, TimeUs::ZERO, &NeverCancel).unwrap();
        assert_eq!(
            renderer
                .render_clip_with_overlay_plan(&frame, &plan, &assets, &NeverCancel)
                .unwrap(),
            direct
        );
        // An accidental PNG/straight-alpha roundtrip would change the first pixel.
        let twice = wpf_pixels::unpremultiply(wpf_pixels::premultiply([0, 0, 254, 253]));
        assert_eq!(twice, [0, 0, 253, 253]);
        assert_ne!(&direct.pixels()[..4], &twice);
        assert_eq!(assets.loads.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn transparent_reference_does_not_erase_current_pixels_and_opaque_reference_replaces_them() {
        let mut current = RgbaSurface::new(size(2, 1), vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        let assets = Assets {
            source: current.clone(),
            snapshot: PremultipliedRgbaSurface::new(size(2, 1), vec![0, 0, 0, 0, 70, 80, 90, 255])
                .unwrap(),
            loads: AtomicUsize::new(0),
        };
        apply(
            &mut current,
            asset(2),
            size(2, 1),
            &assets,
            RenderLimits::default(),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(current.pixels(), &[1, 2, 3, 255, 70, 80, 90, 255]);
        assert_eq!(assets.source.pixels(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn missing_typed_provider_shape_and_working_limits_fail_explicitly() {
        let source = RgbaSurface::new(size(2, 1), vec![0; 8]).unwrap();
        let default_provider =
            |_| -> Result<RgbaSurface, AssetProviderError> { Ok(source.clone()) };
        assert!(matches!(
            CpuRenderer::default().render_clip(&frame(size(2, 1)), &default_provider, &NeverCancel),
            Err(RenderError::CinemagraphSnapshotLoad { .. })
        ));
        let assets = Assets {
            source: source.clone(),
            snapshot: PremultipliedRgbaSurface::new(size(1, 2), vec![0; 8]).unwrap(),
            loads: AtomicUsize::new(0),
        };
        assert!(matches!(
            CpuRenderer::with_limits(RenderLimits {
                max_surface_bytes: 15
            })
            .render_clip(&frame(size(2, 1)), &assets, &NeverCancel),
            Err(RenderError::EffectWorkingMemoryLimitExceeded { .. })
        ));
        assert_eq!(assets.loads.load(Ordering::Relaxed), 0);
        assert!(matches!(
            CpuRenderer::default().render_clip(&frame(size(2, 1)), &assets, &NeverCancel),
            Err(RenderError::CinemagraphSnapshotSizeMismatch { .. })
        ));
        assert_eq!(assets.source, source);
    }

    #[test]
    fn cancelled_composition_does_not_return_a_partial_image_or_mutate_references() {
        struct CancelAfterLoad<'a>(&'a Assets, AtomicBool);
        impl CancellationToken for CancelAfterLoad<'_> {
            fn is_cancelled(&self) -> bool {
                self.0.loads.load(Ordering::Relaxed) > 0 && self.1.swap(true, Ordering::Relaxed)
            }
        }
        let assets = Assets {
            source: RgbaSurface::new(size(4096, 1), vec![30; 16384]).unwrap(),
            snapshot: PremultipliedRgbaSurface::new(size(4096, 1), vec![60; 16384]).unwrap(),
            loads: AtomicUsize::new(0),
        };
        let source = assets.source.clone();
        let snapshot = assets.snapshot.clone();
        assert!(matches!(
            CpuRenderer::default().render_clip(
                &frame(size(4096, 1)),
                &assets,
                &CancelAfterLoad(&assets, AtomicBool::new(false))
            ),
            Err(RenderError::Cancelled)
        ));
        assert_eq!(assets.source, source);
        assert_eq!(assets.snapshot, snapshot);
    }
}
