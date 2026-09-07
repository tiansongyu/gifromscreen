//! A freeze keeps source pixels and paint history; only its baseline is immutable.

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, EditCommand, PhysicalRect, RasterEncoding, validate_raw_rgba_view,
};
use gif_from_screen_editor::{ComposedFrameEdit, edit_composed_frames};
use gif_from_screen_project::AssetStore;

use super::{
    AtomicBool, EditorWorkspace, MAX_FREEZE_FRAMES, MAX_SURFACE_BYTES, MotionProgress,
    check_cancelled, render,
};

impl EditorWorkspace {
    pub(super) fn freeze_region_command(
        &self,
        region: PhysicalRect,
        invert: bool,
        cancellation: &AtomicBool,
        progress: &mut impl FnMut(MotionProgress),
    ) -> Result<(EditCommand, usize), String> {
        check_cancelled(cancellation)?;
        let current = self
            .selection()
            .current()
            .ok_or("Select the frame to use as the frozen image first.")?;
        let selected: Vec<_> = self
            .manifest()
            .timeline
            .frames
            .iter()
            .filter(|frame| self.selection().contains(frame.id))
            .map(|frame| frame.id)
            .collect();
        if selected.is_empty() || selected.len() > MAX_FREEZE_FRAMES {
            return Err("Select between 1 and 1,000 frames for a rectangular freeze.".to_owned());
        }
        let canvas = self.manifest().canvas.size;
        if region.size.validate().is_err() || !region.fits_within(canvas) {
            return Err(
                "The motion rectangle must have nonzero dimensions and fit inside the canvas."
                    .to_owned(),
            );
        }
        if canvas
            .area()
            .and_then(|pixels| pixels.checked_mul(8))
            .is_none_or(|bytes| bytes > MAX_SURFACE_BYTES as u64)
        {
            return Err("The image and frozen baseline exceed the 64 MiB working-memory budget. Resize the animation first.".to_owned());
        }
        let baseline = render(self.active_project(), current, canvas, cancellation)?;
        let baseline_asset = AssetStore::id_for_bytes(baseline.pixels());
        let mut commands = Vec::new();
        if let Some(existing) = self.manifest().assets.get(&baseline_asset) {
            // Raw bytes can legitimately be reused in a different equal-area
            // shape. The step owns its view; never change the registry's shape.
            validate_raw_rgba_view(existing, canvas)?;
        } else {
            commands.push(EditCommand::RegisterAsset {
                asset: AssetDescriptor {
                    id: baseline_asset,
                    byte_len: baseline.pixels().len() as u64,
                    kind: AssetKind::Frame {
                        size: canvas,
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            });
        }
        commands.push(
            edit_composed_frames(
                self.manifest(),
                selected.iter().copied(),
                &ComposedFrameEdit::FreezeRegion {
                    baseline_asset,
                    baseline_size: canvas,
                    region,
                    invert,
                },
            )
            .map_err(|error| error.to_string())?,
        );
        let command = EditCommand::Compound { commands };
        // Validate the complete atomic payload before publishing any new blob.
        self.manifest()
            .clone()
            .apply_command(&command)
            .map_err(|error| error.to_string())?;
        for (index, id) in selected.iter().enumerate() {
            check_cancelled(cancellation)?;
            if *id != current {
                // Preserve the old preflight's source/digest/geometry checks,
                // but do not store another full image for every selected frame.
                render(self.active_project(), *id, canvas, cancellation)?;
            }
            progress(MotionProgress {
                completed: index + 1,
                total: selected.len(),
            });
        }
        check_cancelled(cancellation)?;
        let stored = self
            .active_project()
            .assets()
            .put(baseline.pixels())
            .map_err(|error| error.to_string())?;
        if stored != baseline_asset {
            return Err("The frozen image digest changed while preparing the edit.".to_owned());
        }
        Ok((command, selected.len()))
    }
}
