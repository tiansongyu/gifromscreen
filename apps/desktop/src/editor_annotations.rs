//! Durable annotation edits run on the borrowed workspace's background worker.

use super::{EditorWorkspace, OverlaySelectionAnchor};
use crate::annotation_engine::{
    AnnotationEditReport, AnnotationProgress, check_cancelled, load_annotation_asset,
    prepare_annotations_in_scope, prepare_annotations_with_assets,
};
use gif_from_screen_domain::{AnnotationRequest, EditCommand, TrackId};
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
            .map(|report| report.frames)
    }

    pub(crate) fn apply_annotation_group(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        request: &AnnotationRequest,
        replacing: Option<TrackId>,
        cancellation: &AtomicBool,
        progress: impl FnMut(AnnotationProgress),
    ) -> Result<AnnotationEditReport, String> {
        if !(if replacing.is_some() {
            anchor.matches_project(self)
        } else {
            anchor.matches(self)
        }) {
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
        let coverage = original
            .as_ref()
            .map(|track| match &track.annotation_scope {
                Some(scope) => Ok(scope.clone()),
                None => gif_from_screen_domain::normalize_annotation_scope(
                    &track.items.iter().map(|item| item.span).collect::<Vec<_>>(),
                ),
            })
            .transpose()?;
        let provider =
            |id| load_annotation_asset(self.manifest(), self.active_project().assets(), id);
        let mut prepared = if let Some(scope) = &coverage {
            prepare_annotations_in_scope(
                self.manifest(),
                scope,
                request,
                cancellation,
                progress,
                &provider,
            )?
        } else {
            prepare_annotations_with_assets(
                self.manifest(),
                self.selection().selected(),
                request,
                cancellation,
                progress,
                &provider,
            )?
        };
        if let Some(original) = original {
            if prepared.commands.is_empty() {
                let mut empty = original.clone();
                empty.items.clear();
                empty.annotation = Some(request.clone());
                empty.annotation_scope = coverage;
                empty.opacity = request.opacity;
                prepared
                    .commands
                    .push(EditCommand::UpsertOverlayTrack { track: empty });
            }
            let Some(EditCommand::UpsertOverlayTrack { track }) = prepared.commands.last_mut()
            else {
                return Err("Annotation preparation did not produce a track.".to_owned());
            };
            track.id = original.id;
            track.name = original.name;
            track.visible = original.visible;
            track.blend_mode = original.blend_mode;
        } else if prepared.commands.is_empty() {
            return Err(if prepared.replay_skips.is_empty() {
                "No matching recorded events were found. Choose manual keys, a manual click, or the built-in pointer when the backend has no input metadata.".to_owned()
            } else {
                prepared.replay_skips.message()
            });
        }
        self.commit_annotations(prepared, cancellation)
    }

    fn commit_annotations(
        &mut self,
        prepared: crate::annotation_engine::PreparedAnnotations,
        cancellation: &AtomicBool,
    ) -> Result<AnnotationEditReport, String> {
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
        Ok(AnnotationEditReport {
            frames: prepared.frames,
            replay_skips: prepared.replay_skips,
        })
    }
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

    fn record_first_key(workspace: &mut EditorWorkspace) {
        let commands = workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let mut frame = source.clone();
                frame.capture_binding = CaptureBinding::Original;
                frame.capture_metadata.captured_at = Some(TimeUs::new(index as u64 * 100_000));
                if index == 0 {
                    frame.capture_metadata.key_strokes.push(KeyStroke {
                        physical_key: "KeyA".to_owned(),
                        display_text: Some("A".to_owned()),
                        pressed: true,
                        at: TimeUs::ZERO,
                        repeat: false,
                        modifiers: 0,
                    });
                }
                EditCommand::ReplaceFrame {
                    frame_id: frame.id,
                    replacement: Box::new(frame),
                }
            })
            .collect();
        workspace
            .execute(EditCommand::Compound { commands })
            .unwrap();
        workspace.select_all();
    }

    #[test]
    fn saved_authoring_scope_grows_hold_independently_of_selection_and_survives_undo_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scope.gfsproj");
        let mut workspace = workspace(&path);
        record_first_key(&mut workspace);
        let mut request = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            hold_ms: 1,
            ..AnnotationRequest::default()
        };
        assert_eq!(
            workspace
                .apply_annotation_edit(
                    &workspace.project_edit_anchor(),
                    &request,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .unwrap(),
            1
        );
        let original = workspace.manifest().timeline.overlay_tracks[0].clone();
        assert_eq!(original.annotation_scope.as_ref().unwrap().len(), 3);
        let anchor = workspace.project_edit_anchor();
        workspace.select_only(FrameId::from_u128(3)).unwrap();
        request.hold_ms = 500;
        assert_eq!(
            workspace
                .apply_annotation_group(
                    &anchor,
                    &request,
                    Some(original.id),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .unwrap()
                .frames,
            3
        );
        let updated = workspace.manifest().timeline.overlay_tracks[0].clone();
        assert_eq!(updated.annotation_scope, original.annotation_scope);
        assert_eq!(updated.items.len(), 3);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], original);
        workspace.redo().unwrap();
        drop(workspace);
        let reopened = EditorWorkspace::open(&path, LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().timeline.overlay_tracks[0], updated);
        assert!(reopened.asset_issues().is_empty());
    }

    #[test]
    fn updating_mixed_legacy_or_archived_authoring_scope_is_wholly_atomic() {
        let dir = tempfile::tempdir().unwrap();
        for (index, binding) in [
            CaptureBinding::LegacyUnknown,
            CaptureBinding::ArchivedAfterComposite,
        ]
        .into_iter()
        .enumerate()
        {
            let mut workspace = workspace(&dir.path().join(format!("guard-{index}.gfsproj")));
            record_first_key(&mut workspace);
            let mut request = AnnotationRequest {
                mode: AnnotationMode::RecordedKeys,
                hold_ms: 500,
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
            let group = workspace.manifest().timeline.overlay_tracks[0].id;
            workspace
                .execute(EditCommand::SetCaptureBindings {
                    changes: vec![FrameCaptureBindingChange {
                        frame_id: FrameId::from_u128(2),
                        binding,
                    }],
                })
                .unwrap();
            let before = workspace.manifest().clone();
            let assets = std::fs::read_dir(workspace.active_project().assets().directory())
                .unwrap()
                .count();
            request.foreground.red = 0;
            let error = workspace
                .apply_annotation_group(
                    &workspace.project_edit_anchor(),
                    &request,
                    Some(group),
                    &AtomicBool::new(false),
                    |_| {},
                )
                .unwrap_err();
            assert!(error.contains("whole group is unchanged"));
            assert_eq!(workspace.manifest(), &before);
            assert_eq!(
                std::fs::read_dir(workspace.active_project().assets().directory())
                    .unwrap()
                    .count(),
                assets
            );
            workspace.undo().unwrap();
            assert_eq!(
                workspace.manifest().timeline.frames[1].capture_binding,
                CaptureBinding::Original
            );
            assert_eq!(
                workspace.manifest().timeline.overlay_tracks,
                before.timeline.overlay_tracks
            );
        }
    }

    #[test]
    fn removing_the_only_input_then_updating_clears_old_marks_but_keeps_authoring_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("empty.gfsproj"));
        record_first_key(&mut workspace);
        let request = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            hold_ms: 500,
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
        workspace
            .execute(EditCommand::RemoveFrames {
                frame_ids: vec![FrameId::from_u128(1)],
            })
            .unwrap();
        let before = workspace.manifest().timeline.overlay_tracks[0].clone();
        assert_eq!(before.items.len(), 2);
        let result = workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &request,
                Some(before.id),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(result.frames, 0);
        let updated = &workspace.manifest().timeline.overlay_tracks[0];
        assert!(updated.items.is_empty());
        assert_eq!(updated.annotation_scope, before.annotation_scope);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], before);
    }

    #[test]
    fn legacy_groups_conservatively_preserve_existing_partial_marker_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("legacy.gfsproj"));
        let request = AnnotationRequest {
            mode: AnnotationMode::ManualKeys {
                text: "Legacy".to_owned(),
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
        let mut legacy = workspace.manifest().timeline.overlay_tracks[0].clone();
        legacy.annotation_scope = None;
        legacy.items[0].span = TimelineSpan {
            start: TimeUs::new(25_000),
            duration: DurationUs::new(50_000).unwrap(),
        };
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: legacy.clone(),
            })
            .unwrap();
        workspace.select_only(FrameId::from_u128(2)).unwrap();
        workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &request,
                Some(legacy.id),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let updated = &workspace.manifest().timeline.overlay_tracks[0];
        assert_eq!(
            updated.annotation_scope,
            Some(legacy.items.iter().map(|item| item.span).collect())
        );
        assert_eq!(
            updated
                .items
                .iter()
                .map(|item| item.span)
                .collect::<Vec<_>>(),
            legacy
                .items
                .iter()
                .map(|item| item.span)
                .collect::<Vec<_>>()
        );
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
        track.annotation_scope = Some(track.items.iter().map(|item| item.span).collect());
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
