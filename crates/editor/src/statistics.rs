use std::collections::BTreeSet;

use gif_from_screen_domain::{FrameId, PhysicalSize, ProjectManifest};
use thiserror::Error;

/// Statistics for the current preview/navigation frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentFrameStatistics {
    /// Stable frame identity.
    pub frame_id: FrameId,
    /// One-based position in the current timeline.
    pub frame_number: usize,
    /// Project-relative frame start in microseconds.
    pub start_us: u64,
    /// Frame display duration in microseconds.
    pub duration_us: u64,
}

/// Overflow-safe projection of editor timeline, selection, and asset statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorStatistics {
    /// Total timeline frame count.
    pub frame_count: usize,
    /// Number of distinct selected timeline frames.
    pub selected_frame_count: usize,
    /// Project canvas dimensions.
    pub canvas: PhysicalSize,
    /// Sum of all frame durations in microseconds.
    pub total_duration_us: u64,
    /// Current-frame projection, when navigation has a current frame.
    pub current_frame: Option<CurrentFrameStatistics>,
    /// Sum of distinct selected-frame durations in microseconds.
    pub selection_duration_us: u64,
    /// Smallest frame delay, or `None` for an empty timeline.
    pub minimum_delay_us: Option<u64>,
    /// Largest frame delay, or `None` for an empty timeline.
    pub maximum_delay_us: Option<u64>,
    /// Arithmetic mean rounded to the nearest microsecond, with halves rounded upward.
    pub average_delay_us: Option<u64>,
    /// Sum of every unique manifest asset descriptor's declared byte length.
    pub asset_descriptor_bytes: u64,
    /// Number of unique descriptors in the manifest asset map.
    pub unique_asset_count: usize,
}

/// Typed failure while projecting editor statistics.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EditorStatisticsError {
    /// A selected identity is stale.
    #[error("selected frame {0} does not exist in the timeline")]
    UnknownSelectedFrame(FrameId),
    /// The current navigation identity is stale.
    #[error("current frame {0} does not exist in the timeline")]
    UnknownCurrentFrame(FrameId),
    /// Adding all frame durations exceeded `u64` microseconds.
    #[error("timeline duration exceeds the supported microsecond range")]
    TimelineDurationOverflow,
    /// Adding selected-frame durations exceeded `u64` microseconds.
    #[error("selected-frame duration exceeds the supported microsecond range")]
    SelectionDurationOverflow,
    /// Adding manifest asset byte lengths exceeded `u64`.
    #[error("asset descriptor byte total exceeds the supported range")]
    AssetDescriptorBytesOverflow,
}

/// Computes editor statistics without mutating project or selection state.
///
/// Duplicate selection identities are counted once. The empty timeline is valid and reports zero
/// duration with `None` for current/minimum/maximum/average delay. Asset totals use the manifest's
/// unique descriptor map, so repeated frame references never multiply stored bytes.
///
/// # Errors
///
/// Returns [`EditorStatisticsError`] for stale selection/current identities or checked duration
/// and asset-byte overflow.
pub fn project_statistics(
    project: &ProjectManifest,
    selected: impl IntoIterator<Item = FrameId>,
    current: Option<FrameId>,
) -> Result<EditorStatistics, EditorStatisticsError> {
    let selected: BTreeSet<_> = selected.into_iter().collect();
    let known: BTreeSet<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    if let Some(frame_id) = selected.iter().find(|frame_id| !known.contains(frame_id)) {
        return Err(EditorStatisticsError::UnknownSelectedFrame(*frame_id));
    }
    if let Some(frame_id) = current.filter(|frame_id| !known.contains(frame_id)) {
        return Err(EditorStatisticsError::UnknownCurrentFrame(frame_id));
    }

    let mut total_duration_us = 0_u64;
    let mut selection_duration_us = 0_u64;
    let mut minimum_delay_us = None::<u64>;
    let mut maximum_delay_us = None::<u64>;
    let mut current_frame = None;
    for (index, frame) in project.timeline.frames.iter().enumerate() {
        let duration_us = frame.duration.get();
        if current == Some(frame.id) {
            current_frame = Some(CurrentFrameStatistics {
                frame_id: frame.id,
                frame_number: index + 1,
                start_us: total_duration_us,
                duration_us,
            });
        }
        total_duration_us = total_duration_us
            .checked_add(duration_us)
            .ok_or(EditorStatisticsError::TimelineDurationOverflow)?;
        if selected.contains(&frame.id) {
            selection_duration_us = selection_duration_us
                .checked_add(duration_us)
                .ok_or(EditorStatisticsError::SelectionDurationOverflow)?;
        }
        minimum_delay_us =
            Some(minimum_delay_us.map_or(duration_us, |value| value.min(duration_us)));
        maximum_delay_us =
            Some(maximum_delay_us.map_or(duration_us, |value| value.max(duration_us)));
    }
    let average_delay_us = if project.timeline.frames.is_empty() {
        None
    } else {
        let count = u128::try_from(project.timeline.frames.len())
            .map_err(|_| EditorStatisticsError::TimelineDurationOverflow)?;
        let average = u128::from(total_duration_us)
            .checked_add(count / 2)
            .ok_or(EditorStatisticsError::TimelineDurationOverflow)?
            / count;
        Some(u64::try_from(average).map_err(|_| EditorStatisticsError::TimelineDurationOverflow)?)
    };
    let asset_descriptor_bytes = project.assets.values().try_fold(0_u64, |total, asset| {
        total
            .checked_add(asset.byte_len)
            .ok_or(EditorStatisticsError::AssetDescriptorBytesOverflow)
    })?;

    Ok(EditorStatistics {
        frame_count: project.timeline.frames.len(),
        selected_frame_count: selected.len(),
        canvas: project.canvas.size,
        total_duration_us,
        current_frame,
        selection_duration_us,
        minimum_delay_us,
        maximum_delay_us,
        average_delay_us,
        asset_descriptor_bytes,
        unique_asset_count: project.assets.len(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, DurationUs, FrameClip, ProjectId, ProjectRevision,
        RasterEncoding, Timeline, UnixTimeMs,
    };

    use super::*;

    fn project(durations: &[u64]) -> ProjectManifest {
        let size = PhysicalSize::new(4, 3).unwrap();
        let first_asset = AssetId::from_digest([1; 32]);
        let second_asset = AssetId::from_digest([2; 32]);
        let assets = BTreeMap::from([
            (
                first_asset,
                AssetDescriptor {
                    id: first_asset,
                    byte_len: 48,
                    kind: AssetKind::Frame {
                        size,
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            ),
            (
                second_asset,
                AssetDescriptor {
                    id: second_asset,
                    byte_len: 24,
                    kind: AssetKind::ImportedSource {
                        media_type: "image/png".to_owned(),
                    },
                },
            ),
        ]);
        let frames = durations
            .iter()
            .copied()
            .enumerate()
            .map(|(index, duration)| FrameClip {
                id: FrameId::from_u128(u128::try_from(index).unwrap() + 1),
                asset_id: first_asset,
                duration: DurationUs::new(duration).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect();
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "statistics-test".to_owned(),
            created_at: UnixTimeMs::new(1),
            canvas: Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
            timeline: Timeline {
                frames,
                ..Timeline::default()
            },
            assets,
            export_presets: BTreeMap::new(),
            task_runs: Vec::new(),
            source_provenance: Vec::new(),
        }
    }

    #[test]
    fn variable_timing_selection_current_and_repeated_assets_are_exact() {
        let project = project(&[10, 20, 40]);
        let statistics = project_statistics(
            &project,
            [
                FrameId::from_u128(3),
                FrameId::from_u128(1),
                FrameId::from_u128(3),
            ],
            Some(FrameId::from_u128(2)),
        )
        .unwrap();
        assert_eq!(statistics.frame_count, 3);
        assert_eq!(statistics.selected_frame_count, 2);
        assert_eq!(statistics.canvas, PhysicalSize::new(4, 3).unwrap());
        assert_eq!(statistics.total_duration_us, 70);
        assert_eq!(statistics.selection_duration_us, 50);
        assert_eq!(statistics.minimum_delay_us, Some(10));
        assert_eq!(statistics.maximum_delay_us, Some(40));
        assert_eq!(statistics.average_delay_us, Some(23));
        assert_eq!(statistics.unique_asset_count, 2);
        assert_eq!(statistics.asset_descriptor_bytes, 72);
        assert_eq!(
            statistics.current_frame,
            Some(CurrentFrameStatistics {
                frame_id: FrameId::from_u128(2),
                frame_number: 2,
                start_us: 10,
                duration_us: 20,
            })
        );
    }

    #[test]
    fn empty_timeline_has_explicit_zero_and_optional_statistics() {
        let project = project(&[]);
        let statistics = project_statistics(&project, [], None).unwrap();
        assert_eq!(statistics.frame_count, 0);
        assert_eq!(statistics.selected_frame_count, 0);
        assert_eq!(statistics.total_duration_us, 0);
        assert_eq!(statistics.selection_duration_us, 0);
        assert_eq!(statistics.current_frame, None);
        assert_eq!(statistics.minimum_delay_us, None);
        assert_eq!(statistics.maximum_delay_us, None);
        assert_eq!(statistics.average_delay_us, None);
    }

    #[test]
    fn stale_identities_and_all_overflow_paths_are_typed() {
        let baseline = project(&[10]);
        assert_eq!(
            project_statistics(&baseline, [FrameId::from_u128(99)], None),
            Err(EditorStatisticsError::UnknownSelectedFrame(
                FrameId::from_u128(99)
            ))
        );
        assert_eq!(
            project_statistics(&baseline, [], Some(FrameId::from_u128(99))),
            Err(EditorStatisticsError::UnknownCurrentFrame(
                FrameId::from_u128(99)
            ))
        );

        let duration_overflow = project(&[u64::MAX, 1]);
        assert_eq!(
            project_statistics(&duration_overflow, [], None),
            Err(EditorStatisticsError::TimelineDurationOverflow)
        );

        let mut asset_overflow = project(&[1]);
        let mut assets = asset_overflow.assets.values_mut();
        assets.next().unwrap().byte_len = u64::MAX;
        assets.next().unwrap().byte_len = 1;
        assert_eq!(
            project_statistics(&asset_overflow, [], None),
            Err(EditorStatisticsError::AssetDescriptorBytesOverflow)
        );
    }
}
