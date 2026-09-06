//! Durable annotation edits run on the borrowed workspace's background worker.

use super::{EditorWorkspace, OverlaySelectionAnchor};
use crate::annotation_engine::{
    AnnotationProgress, check_cancelled, load_annotation_asset, prepare_annotations_with_assets,
};
use gif_from_screen_domain::{
    AnnotationRequest, DurationUs, EditCommand, OverlayId, TimeUs, TimelineSpan, TrackId,
};
use std::sync::atomic::AtomicBool;

impl EditorWorkspace {
    pub(crate) fn apply_annotation_edit(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        request: &AnnotationRequest,
        cancellation: &AtomicBool,
        progress: impl FnMut(AnnotationProgress),
    ) -> Result<usize, String> {
        self.apply_annotation_group(anchor, request, None, cancellation, progress)
    }

    pub(crate) fn apply_annotation_group(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        request: &AnnotationRequest,
        replacing: Option<TrackId>,
        cancellation: &AtomicBool,
        progress: impl FnMut(AnnotationProgress),
    ) -> Result<usize, String> {
        if !anchor.matches(self) {
            return Err("Project or selection changed before annotation preparation.".to_owned());
        }
        let original = replacing
            .map(|id| {
                self.manifest()
                    .timeline
                    .overlay_tracks
                    .iter()
                    .find(|track| track.id == id)
                    .filter(|track| track.annotation.is_some())
                    .cloned()
                    .ok_or_else(|| {
                        "The original annotation group is no longer available.".to_owned()
                    })
            })
            .transpose()?;
        let mut selected = self.selection().selected().clone();
        let coverage = original
            .as_ref()
            .map(|track| merged_spans(track.items.iter().map(|item| item.span)))
            .transpose()?;
        if let Some(coverage) = &coverage {
            selected.clear();
            let mut start = 0_u64;
            for frame in &self.manifest().timeline.frames {
                let end = start + frame.duration.get();
                let index = coverage.partition_point(|(_, right)| *right <= start);
                if coverage.get(index).is_some_and(|(left, _)| *left < end) {
                    selected.insert(frame.id);
                }
                start = end;
            }
        }
        let mut prepared = prepare_annotations_with_assets(
            self.manifest(),
            &selected,
            request,
            cancellation,
            progress,
            &|id| load_annotation_asset(self.manifest(), self.active_project().assets(), id),
        )?;
        if prepared.commands.is_empty() {
            return Err("No matching recorded events were found. Choose manual keys, a manual click, or the built-in pointer when the backend has no input metadata.".to_owned());
        }
        if let Some(original) = original {
            let Some(EditCommand::UpsertOverlayTrack { track }) = prepared.commands.last_mut()
            else {
                return Err("Annotation preparation did not produce a track.".to_owned());
            };
            track.id = original.id;
            track.name = original.name;
            track.visible = original.visible;
            track.blend_mode = original.blend_mode;
            let mut clipped = Vec::new();
            for item in &track.items {
                let end = item
                    .span
                    .end()
                    .ok_or_else(|| "Annotation coverage overflows time.".to_owned())?
                    .get();
                let coverage = coverage.as_ref().expect("original has coverage");
                let index = coverage.partition_point(|(_, right)| *right <= item.span.start.get());
                for (left, right) in coverage[index..].iter().take_while(|(left, _)| *left < end) {
                    let start = item.span.start.get().max(*left);
                    let end = end.min(*right);
                    if let Some(duration) = end.checked_sub(start).and_then(DurationUs::new) {
                        if clipped.len() >= 40_000 {
                            return Err(
                                "Editing this annotation would exceed 40,000 fragments.".to_owned()
                            );
                        }
                        let mut next = item.clone();
                        next.id = OverlayId::from_u128(uuid::Uuid::new_v4().as_u128());
                        next.span = TimelineSpan {
                            start: TimeUs::new(start),
                            duration,
                        };
                        clipped.push(next);
                    }
                }
            }
            track.items = clipped;
        }
        self.commit_annotations(prepared, cancellation)
    }

    fn commit_annotations(
        &mut self,
        prepared: crate::annotation_engine::PreparedAnnotations,
        cancellation: &AtomicBool,
    ) -> Result<usize, String> {
        check_cancelled(cancellation)?;
        let command = EditCommand::Compound {
            commands: prepared.commands,
        };
        self.manifest()
            .clone()
            .apply_command(&command)
            .map_err(|error| error.to_string())?;
        for (descriptor, bytes) in &prepared.assets {
            check_cancelled(cancellation)?;
            let stored = self
                .active_project()
                .assets()
                .put(bytes)
                .map_err(|error| error.to_string())?;
            if stored != descriptor.id {
                return Err("Annotation asset identity changed during storage.".to_owned());
            }
        }
        check_cancelled(cancellation)?;
        self.execute(command).map_err(|error| error.to_string())?;
        let repaired: std::collections::BTreeSet<_> =
            prepared.assets.iter().map(|(asset, _)| asset.id).collect();
        self.asset_issues
            .retain(|issue| !repaired.contains(&super::asset_issue_id(issue)));
        Ok(prepared.frames)
    }
}

fn merged_spans(spans: impl Iterator<Item = TimelineSpan>) -> Result<Vec<(u64, u64)>, String> {
    let mut spans: Vec<_> = spans
        .map(|span| {
            Ok((
                span.start.get(),
                span.end()
                    .ok_or_else(|| "Annotation span overflow.".to_owned())?
                    .get(),
            ))
        })
        .collect::<Result<_, String>>()?;
    spans.sort_unstable();
    let mut result: Vec<(u64, u64)> = Vec::new();
    for (left, right) in spans {
        if let Some(last) = result.last_mut()
            && left <= last.1
        {
            last.1 = last.1.max(right);
            continue;
        }
        result.push((left, right));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_preview::render_frame_surface;
    use gif_from_screen_application::{
        IncrementalRecordingProject, IncrementalRecordingProjectOptions,
    };
    use gif_from_screen_domain::*;
    use gif_from_screen_gif::RgbaFrame;
    use gif_from_screen_project::LockPolicy;

    fn workspace(root: &std::path::Path) -> EditorWorkspace {
        let mut writer = IncrementalRecordingProject::create(
            root,
            PhysicalSize::new(240, 40).unwrap(),
            IncrementalRecordingProjectOptions {
                project_id: ProjectId::from_u128(92),
                app_version: "annotation-test".to_owned(),
                created_at: UnixTimeMs::new(0),
                source_label: None,
            },
        )
        .unwrap();
        for index in 0..3 {
            writer
                .append_frame(
                    FrameId::from_u128(index + 1),
                    &RgbaFrame::new(240, 40, [0, 0, 0, 0].repeat(240 * 40), 100_000).unwrap(),
                )
                .unwrap();
        }
        let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 16).unwrap();
        workspace.select_only(FrameId::from_u128(1)).unwrap();
        workspace.toggle_selection(FrameId::from_u128(3)).unwrap();
        workspace
    }

    #[test]
    fn labels_preserve_preview_pixels_through_undo_reopen_and_export_transparent_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("annotations.gfsproj");
        let mut workspace = workspace(&root);
        let before = workspace.manifest().clone();
        let request = AnnotationRequest {
            mode: AnnotationMode::ManualKeys {
                text: "Ctrl+C".to_owned(),
            },
            ..AnnotationRequest::default()
        };
        workspace
            .apply_annotation_edit(
                &workspace.project_edit_anchor(),
                &request,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let first = render_frame_surface(
            workspace.active_project(),
            FrameId::from_u128(1),
            1024 * 1024,
        )
        .unwrap();
        assert!(
            first
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        let gap = render_frame_surface(
            workspace.active_project(),
            FrameId::from_u128(2),
            1024 * 1024,
        )
        .unwrap();
        assert!(gap.pixels().iter().all(|byte| *byte == 0));
        workspace.undo().unwrap();
        let mut restored = workspace.manifest().clone();
        restored.revision = before.revision;
        assert_eq!(restored, before);
        workspace.redo().unwrap();
        let expected = workspace.manifest().timeline.overlay_tracks.clone();
        drop(workspace);
        let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 16).unwrap();
        assert!(reopened.asset_issues().is_empty());
        assert_eq!(reopened.manifest().timeline.overlay_tracks, expected);
        assert_eq!(
            render_frame_surface(
                reopened.active_project(),
                FrameId::from_u128(1),
                1024 * 1024
            )
            .unwrap()
            .pixels(),
            first.pixels()
        );
        let output = dir.path().join("annotations.gif");
        gif_from_screen_application::export_project_snapshot_to_gif(
            &gif_from_screen_application::ProjectExportSnapshot::from_active(
                reopened.active_project(),
            ),
            &output,
            &gif_from_screen_application::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut gif_from_screen_application::NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            std::fs::File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(decoded.frames().len(), 3);
        // GIF alpha is binary. Presence, timing, and the transparent selection gap are exact.
        assert!(
            decoded.frames()[0]
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        assert!(
            decoded.frames()[1]
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0)
        );
    }

    #[test]
    fn editing_a_saved_group_preserves_partial_spans_and_ignores_new_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("edit.gfsproj"));
        let mut request = AnnotationRequest {
            mode: AnnotationMode::ManualKeys {
                text: "Ctrl+C".to_owned(),
            },
            ..AnnotationRequest::default()
        };
        workspace
            .apply_annotation_edit(
                &workspace.project_edit_anchor(),
                &request,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let mut track = workspace.manifest().timeline.overlay_tracks[0].clone();
        track.items[0].span = TimelineSpan {
            start: TimeUs::new(25_000),
            duration: DurationUs::new(50_000).unwrap(),
        };
        track.visible = false;
        track.name = "Saved shortcut".to_owned();
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: track.clone(),
            })
            .unwrap();
        workspace.select_only(FrameId::from_u128(2)).unwrap();
        request.mode = AnnotationMode::ManualKeys {
            text: "Ctrl+V".to_owned(),
        };
        workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &request,
                Some(track.id),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let updated = &workspace.manifest().timeline.overlay_tracks[0];
        assert_eq!(updated.id, track.id);
        assert_eq!(updated.name, track.name);
        assert!(!updated.visible);
        assert_eq!(updated.annotation, Some(request));
        assert_eq!(
            updated
                .items
                .iter()
                .map(|item| item.span)
                .collect::<Vec<_>>(),
            track.items.iter().map(|item| item.span).collect::<Vec<_>>()
        );
        assert!(
            matches!(&updated.items[0].content,OverlayContent::KeyStroke{text,..} if text=="Ctrl+V")
        );
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], track);
    }

    #[test]
    fn cancelled_or_stale_edit_preserves_project_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("project.gfsproj"));
        let before = workspace.manifest().clone();
        let anchor = workspace.project_edit_anchor();
        assert!(
            workspace
                .apply_annotation_edit(
                    &anchor,
                    &AnnotationRequest::default(),
                    &AtomicBool::new(true),
                    |_| {}
                )
                .is_err()
        );
        assert_eq!(workspace.manifest(), &before);
        assert!(!workspace.can_undo());
        let no_events = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        };
        assert!(
            workspace
                .apply_annotation_edit(&anchor, &no_events, &AtomicBool::new(false), |_| {})
                .unwrap_err()
                .contains("No matching recorded events")
        );
        assert_eq!(workspace.manifest(), &before);
        assert!(!workspace.can_undo());
        workspace.select_only(FrameId::from_u128(2)).unwrap();
        assert!(
            workspace
                .apply_annotation_edit(
                    &anchor,
                    &AnnotationRequest::default(),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
        );
        assert_eq!(workspace.manifest(), &before);
    }
}
