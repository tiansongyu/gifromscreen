//! First-frame ink clipping and one immutable PM reference, committed atomically.

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, EditCommand, PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION,
    PREMULTIPLIED_SNAPSHOT_HEADER_LEN, validate_premultiplied_snapshot_view,
};
use gif_from_screen_editor::{ComposedFrameEdit, edit_composed_frames};
use gif_from_screen_project::AssetStore;
use gif_from_screen_render::{InkFigure, InkPath, InkSample, InkSegment, InkStroke};
use gif_from_screen_render::{InkLimits, clip_ink_reference, outline_ink_strokes};

use super::{
    AtomicBool, Cancellation, EditorWorkspace, MAX_SURFACE_BYTES, MotionProgress, check_cancelled,
    render,
};
use crate::cinemagraph_draft::{
    CinemagraphRequest, MAX_CINEMAGRAPH_SAMPLES, MAX_CINEMAGRAPH_STROKES, MAX_CINEMAGRAPH_TARGETS,
};

impl EditorWorkspace {
    pub(super) fn cinemagraph_command(
        &self,
        request: &CinemagraphRequest,
        cancellation: &AtomicBool,
        progress: &mut impl FnMut(MotionProgress),
    ) -> Result<(EditCommand, usize), String> {
        check_cancelled(cancellation)?;
        let selected = self.cinemagraph_targets(request)?;
        // Render each selected source before publishing a new reference. This
        // retains exact size, digest and unsupported-operation preflight.
        for (index, id) in selected.iter().enumerate() {
            check_cancelled(cancellation)?;
            render(
                self.active_project(),
                *id,
                request.reference_size,
                cancellation,
            )?;
            progress(MotionProgress {
                completed: index,
                total: selected.len(),
            });
        }
        let encoded = self.render_cinemagraph_snapshot(request, cancellation)?;
        let id = AssetStore::id_for_bytes(&encoded);
        let mut commands = Vec::new();
        if let Some(existing) = self.manifest().assets.get(&id) {
            validate_premultiplied_snapshot_view(existing, request.reference_size)?;
        } else {
            commands.push(EditCommand::RegisterAsset {
                asset: AssetDescriptor {
                    id,
                    byte_len: encoded.len() as u64,
                    kind: AssetKind::PremultipliedSnapshot {
                        size: request.reference_size,
                        format_version: PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION,
                    },
                },
            });
        }
        commands.push(
            edit_composed_frames(
                self.manifest(),
                selected.iter().copied(),
                &ComposedFrameEdit::CinemagraphOverlay {
                    snapshot_asset: id,
                    snapshot_size: request.reference_size,
                },
            )
            .map_err(|error| error.to_string())?,
        );
        let command = EditCommand::Compound { commands };
        self.manifest()
            .clone()
            .apply_command(&command)
            .map_err(|error| error.to_string())?;
        check_cancelled(cancellation)?;
        if self
            .active_project()
            .assets()
            .put(&encoded)
            .map_err(|error| error.to_string())?
            != id
        {
            return Err("Cinemagraph snapshot identity changed during storage.".to_owned());
        }
        progress(MotionProgress {
            completed: selected.len(),
            total: selected.len(),
        });
        Ok((command, selected.len()))
    }
    fn cinemagraph_targets(
        &self,
        request: &CinemagraphRequest,
    ) -> Result<Vec<gif_from_screen_domain::FrameId>, String> {
        if !request.anchor.matches(self)
            || self
                .manifest()
                .timeline
                .frames
                .first()
                .is_none_or(|frame| frame.id != request.reference_frame)
        {
            return Err(
                "The Cinemagraph reference or targets changed; the draft was not applied."
                    .to_owned(),
            );
        }
        if request.strokes.is_empty() || request.strokes.len() > MAX_CINEMAGRAPH_STROKES {
            return Err("Draw a motion region first, using at most 256 strokes.".to_owned());
        }
        let samples = request
            .strokes
            .iter()
            .try_fold(0usize, |count, stroke| {
                count.checked_add(stroke.samples.len())
            })
            .ok_or("Cinemagraph sample count overflow.")?;
        if samples > MAX_CINEMAGRAPH_SAMPLES {
            return Err("Cinemagraph exceeds its bounded sample count.".to_owned());
        }
        if request.strokes.iter().any(|stroke| {
            stroke.samples.is_empty()
                || !(1.0..=100.0).contains(&stroke.attributes.width)
                || !(1.0..=100.0).contains(&stroke.attributes.height)
                || stroke.samples.iter().any(|sample| {
                    !sample.position.x.is_finite()
                        || !sample.position.y.is_finite()
                        || !(0.0..=1.0).contains(&sample.pressure)
                })
        }) {
            return Err(
                "Cinemagraph contains invalid pen dimensions, samples or pressure.".to_owned(),
            );
        }
        let selected: Vec<_> = self
            .manifest()
            .timeline
            .frames
            .iter()
            .filter(|frame| self.selection().contains(frame.id))
            .map(|frame| frame.id)
            .collect();
        if selected.is_empty() || selected.len() > MAX_CINEMAGRAPH_TARGETS {
            return Err("Select between 1 and 1,000 target frames.".to_owned());
        }
        if request
            .reference_size
            .area()
            .and_then(|pixels| pixels.checked_mul(8))
            .and_then(|bytes| bytes.checked_add(PREMULTIPLIED_SNAPSHOT_HEADER_LEN as u64))
            .is_none_or(|bytes| bytes > MAX_SURFACE_BYTES as u64)
        {
            return Err(
                "The Cinemagraph reference exceeds the 64 MiB working-image budget.".to_owned(),
            );
        }
        Ok(selected)
    }

    fn render_cinemagraph_snapshot(
        &self,
        request: &CinemagraphRequest,
        cancellation: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        let request_bytes = request
            .strokes
            .capacity()
            .checked_mul(std::mem::size_of::<InkStroke>())
            .and_then(|bytes| {
                request.strokes.iter().try_fold(bytes, |total, stroke| {
                    stroke
                        .samples
                        .capacity()
                        .checked_mul(std::mem::size_of::<InkSample>())
                        .and_then(|bytes| total.checked_add(bytes))
                })
            })
            .ok_or("Cinemagraph request memory size overflow.")?;
        let snapshot = {
            let source = render(
                self.active_project(),
                request.reference_frame,
                request.reference_size,
                cancellation,
            )?;
            let available = MAX_SURFACE_BYTES
                .checked_sub(request_bytes)
                .and_then(|bytes| bytes.checked_sub(source.pixels().len()))
                .ok_or("Cinemagraph reference and request exceed the geometry budget.")?;
            let limits = InkLimits {
                max_bytes: available,
                ..InkLimits::default()
            };
            let paths = outline_ink_strokes(&request.strokes, &limits, &Cancellation(cancellation))
                .map_err(|error| error.to_string())?;
            let retained = path_bytes(&paths)?
                .checked_add(request_bytes)
                .ok_or("Cinemagraph retained geometry byte count overflow.")?;
            // The clipper accounts for source, PM output and scan scratch;
            // deduct the paths/request that stay alive around that operation.
            let clip_limits = InkLimits {
                max_bytes: MAX_SURFACE_BYTES
                    .checked_sub(retained)
                    .ok_or("Cinemagraph paths exceed the working-memory budget.")?,
                ..InkLimits::default()
            };
            clip_ink_reference(&source, &paths, &clip_limits, &Cancellation(cancellation))
                .map_err(|error| error.to_string())?
        };
        let encoded_budget = MAX_SURFACE_BYTES
            .checked_sub(request_bytes)
            .and_then(|bytes| bytes.checked_sub(snapshot.pixels().len()))
            .ok_or("Cinemagraph encoding exceeds its working-memory budget.")?;
        let encoded = snapshot
            .encode(encoded_budget)
            .map_err(|error| error.to_string())?;
        drop(snapshot);
        Ok(encoded)
    }
}

fn path_bytes(paths: &Vec<InkPath>) -> Result<usize, String> {
    let mut bytes = paths
        .capacity()
        .checked_mul(std::mem::size_of::<InkPath>())
        .ok_or("Cinemagraph path storage overflow.")?;
    for path in paths {
        bytes = bytes
            .checked_add(
                path.figures
                    .capacity()
                    .checked_mul(std::mem::size_of::<InkFigure>())
                    .ok_or("Cinemagraph figure storage overflow.")?,
            )
            .ok_or("Cinemagraph path storage overflow.")?;
        for figure in &path.figures {
            bytes = bytes
                .checked_add(
                    figure
                        .segments
                        .capacity()
                        .checked_mul(std::mem::size_of::<InkSegment>())
                        .ok_or("Cinemagraph segment storage overflow.")?,
                )
                .ok_or("Cinemagraph path storage overflow.")?;
        }
    }
    Ok(bytes)
}
