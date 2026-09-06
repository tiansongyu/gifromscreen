//! A pull renderer that retains only the current transition's endpoint surfaces.

use std::{collections::BTreeMap, path::Path};

use gif_from_screen_domain::{FrameClip, TimeUs};
use gif_from_screen_gif::{
    CancellationToken as GifCancellationToken, EncodeReport, FrameSourceError, RgbaFrame,
    RgbaFrameSource, Transparency,
};
use gif_from_screen_render::{CpuRenderer, RenderLimits, RgbaSurface, render_transition};

use super::{
    ExportExecution, LoadedAssetProvider, ProjectExportPhase, ProjectExportSnapshot,
    ProjectGifExportError, ProjectGifExportOptions, RenderCancellationAdapter,
    encode_source_and_commit, encoder_for_options, ensure_not_cancelled, ensure_render_buffer,
    first_frame_requiring_transparency, load_selected_assets, surface_to_gif_frame,
};
use crate::{PresentationPlan, transition_step_progress};

#[allow(
    clippy::too_many_arguments,
    reason = "the immutable render plan and atomic output context remain explicit"
)]
pub(super) fn encode_local(
    snapshot: &ProjectExportSnapshot,
    clips: &[FrameClip],
    frame_times: &[TimeUs],
    presentation: &PresentationPlan,
    output: &Path,
    parent: &Path,
    options: &ProjectGifExportOptions,
    execution: &mut ExportExecution<'_>,
) -> Result<(EncodeReport, u64), ProjectGifExportError> {
    // Palette shape is validated up front; missing transparency is checked on
    // each rendered frame so custom palettes also remain genuinely streaming.
    let encoder = encoder_for_options(options, &[], execution.cancellation)?;
    let mut source = RenderedFrameSource {
        snapshot,
        clips,
        frame_times,
        presentation,
        options,
        cancellation: execution.cancellation,
        cached: BTreeMap::new(),
        next_time_us: 0,
        frames_rendered: 0,
        prefetched: None,
        failure: None,
    };
    execution.report_phase(ProjectExportPhase::Rendering);
    source.prefetched = source.render_next()?;
    execution.state.frames_rendered = source.frames_rendered;
    execution.progress.report(execution.state);
    let result = encode_source_and_commit(
        &encoder,
        &mut source,
        output,
        parent,
        options,
        execution,
        true,
    );
    // Preserve application-level asset/geometry errors instead of flattening
    // them into the encoder's object-safe frame-source error boundary.
    source.failure.take().map_or(result, Err)
}

/// Reconstructing this source from the immutable plan is a deterministic replay.
/// Pixels are never stored in the plan or retained for the complete animation.
struct RenderedFrameSource<'a> {
    snapshot: &'a ProjectExportSnapshot,
    clips: &'a [FrameClip],
    frame_times: &'a [TimeUs],
    presentation: &'a PresentationPlan,
    options: &'a ProjectGifExportOptions,
    cancellation: &'a dyn GifCancellationToken,
    cached: BTreeMap<usize, RgbaSurface>,
    next_time_us: u64,
    frames_rendered: u64,
    prefetched: Option<RgbaFrame>,
    failure: Option<ProjectGifExportError>,
}

impl RgbaFrameSource for RenderedFrameSource<'_> {
    fn next_frame(&mut self) -> Result<Option<RgbaFrame>, FrameSourceError> {
        if self.failure.is_some() {
            return Err(FrameSourceError::new("project frame source already failed"));
        }
        if let Some(frame) = self.prefetched.take() {
            return Ok(Some(frame));
        }
        self.render_next().map_err(|error| {
            let message = error.to_string();
            self.failure = Some(error);
            FrameSourceError::new(message)
        })
    }

    fn frame_count_hint(&self) -> Option<u64> {
        Some(self.presentation.frame_count())
    }
}

impl RenderedFrameSource<'_> {
    fn render_next(&mut self) -> Result<Option<RgbaFrame>, ProjectGifExportError> {
        ensure_not_cancelled(self.cancellation)?;
        let Some(sample) = self.presentation.sample_at_us(self.next_time_us) else {
            self.cached.clear();
            return Ok(None);
        };
        let index = sample.frame_index;
        self.cached.retain(|cached_index, _| {
            *cached_index == index || (sample.transition.is_some() && *cached_index == index + 1)
        });
        let surface = if let Some(step) = sample.transition {
            self.render_intermediate(index, step.step)?
        } else {
            let surface = match self.cached.remove(&index) {
                Some(surface) => surface,
                None => self.render_original(index)?,
            };
            if self
                .presentation
                .transitions()
                .get(index)
                .is_some_and(Option::is_some)
            {
                let bytes = surface_bytes(&surface);
                ensure_render_buffer(0, self.cached_bytes()?, &[bytes, bytes], self.limit())?;
                self.cached.insert(index, surface.clone());
            }
            surface
        };
        let frame = surface_to_gif_frame(surface, sample.frame_id, sample.duration.get())?;
        self.validate_custom_transparency(&frame)?;
        self.next_time_us = self
            .next_time_us
            .checked_add(sample.duration.get())
            .ok_or(ProjectGifExportError::OutputDurationOverflow)?;
        self.frames_rendered += 1;
        Ok(Some(frame))
    }

    fn render_intermediate(
        &mut self,
        index: usize,
        step: u16,
    ) -> Result<RgbaSurface, ProjectGifExportError> {
        // Original output always precedes its transition. Incoming output can
        // reuse the second endpoint without re-rendering it after the transition.
        if !self.cached.contains_key(&(index + 1)) {
            let incoming = self.render_original(index + 1)?;
            self.cached.insert(index + 1, incoming);
        }
        let transition = self.presentation.transitions()[index]
            .as_ref()
            .expect("presentation intermediate has a transition");
        let from = self
            .cached
            .get(&index)
            .expect("original transition endpoint is cached");
        let to = self
            .cached
            .get(&(index + 1))
            .expect("incoming transition endpoint is cached");
        ensure_render_buffer(
            0,
            self.cached_bytes()?,
            &[surface_bytes(from)],
            self.limit(),
        )?;
        let progress = transition_step_progress(transition, step).map_err(|source| {
            ProjectGifExportError::RenderTransition {
                transition: transition.clone(),
                step,
                source: Box::new(source),
            }
        })?;
        render_transition(
            from,
            to,
            &transition.kind,
            progress,
            &RenderCancellationAdapter(self.cancellation),
        )
        .map_err(|source| {
            if self.cancellation.is_cancelled() {
                ProjectGifExportError::Cancelled
            } else {
                ProjectGifExportError::RenderTransition {
                    transition: transition.clone(),
                    step,
                    source: Box::new(source),
                }
            }
        })
    }

    fn render_original(&self, index: usize) -> Result<RgbaSurface, ProjectGifExportError> {
        let clip = &self.clips[index];
        let time = self.frame_times[index];
        let retained = self.cached_bytes()?;
        let available = self.limit().checked_sub(retained).ok_or(
            ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes: retained,
                limit_bytes: self.limit(),
            },
        )?;
        // Reuse the existing descriptor/digest/active-overlay checks, but load
        // only the assets needed for this one original frame's source time.
        let (assets, source_bytes) = load_selected_assets(
            self.snapshot,
            std::slice::from_ref(clip),
            std::slice::from_ref(&time),
            available,
            self.cancellation,
        )?;
        let renderer = CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: usize::try_from(available).unwrap_or(usize::MAX),
        });
        let surface = renderer
            .render_clip_with_overlays(
                clip,
                &self.snapshot.manifest.timeline.overlay_tracks,
                time,
                &LoadedAssetProvider { assets },
                &RenderCancellationAdapter(self.cancellation),
            )
            .map_err(|source| {
                if self.cancellation.is_cancelled() {
                    ProjectGifExportError::Cancelled
                } else {
                    ProjectGifExportError::RenderFrame {
                        selection_index: index,
                        frame_id: clip.id,
                        source,
                    }
                }
            })?;
        ensure_render_buffer(
            source_bytes,
            retained,
            &[surface_bytes(&surface)],
            self.limit(),
        )?;
        Ok(surface)
    }

    fn validate_custom_transparency(&self, frame: &RgbaFrame) -> Result<(), ProjectGifExportError> {
        if let Some(palette) = &self.options.custom_palette
            && palette.transparent_index().is_none()
            && let Transparency::AlphaThreshold(alpha_threshold) =
                self.options.encoding.transparency
            && first_frame_requiring_transparency(
                std::slice::from_ref(frame),
                alpha_threshold,
                self.cancellation,
            )?
            .is_some()
        {
            return Err(ProjectGifExportError::CustomPaletteMissingTransparency {
                frame_index: usize::try_from(self.frames_rendered)
                    .map_err(|_| ProjectGifExportError::OutputFrameCountOverflow)?,
                alpha_threshold,
            });
        }
        Ok(())
    }

    fn cached_bytes(&self) -> Result<u64, ProjectGifExportError> {
        self.cached.values().try_fold(0_u64, |sum, surface| {
            sum.checked_add(surface_bytes(surface))
                .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)
        })
    }

    const fn limit(&self) -> u64 {
        self.options.render_buffer_limit_bytes
    }
}

fn surface_bytes(surface: &RgbaSurface) -> u64 {
    u64::try_from(surface.pixels().len()).unwrap_or(u64::MAX)
}
