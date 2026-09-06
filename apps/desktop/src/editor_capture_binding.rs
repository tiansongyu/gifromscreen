//! Explicit user confirmation for legacy input associations; composited frames cannot be relabeled.

use gif_from_screen_domain::{CaptureBinding, EditCommand, FrameCaptureBindingChange};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{EditorWorkspace, OverlaySelectionAnchor};

impl EditorWorkspace {
    /// Run on the editor worker after the user verifies that legacy pixels are still original.
    pub(crate) fn confirm_original_capture_binding(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        cancellation: &AtomicBool,
    ) -> Result<usize, String> {
        if cancellation.load(Ordering::Acquire) {
            return Err("Original-coordinate confirmation cancelled before saving.".to_owned());
        }
        if !anchor.matches(self) {
            return Err(
                "The project or selection changed before confirming original capture coordinates."
                    .to_owned(),
            );
        }
        if self.selection().is_empty() {
            return Err("Select the legacy frames whose original pixels you verified.".to_owned());
        }
        if self.selection().selected().len() > 100_000 {
            return Err("Confirm at most 100,000 original frames at a time.".to_owned());
        }
        let mut changes = Vec::new();
        let mut has_input = false;
        for frame in self
            .manifest()
            .timeline
            .frames
            .iter()
            .filter(|frame| self.selection().contains(frame.id))
        {
            if cancellation.load(Ordering::Acquire) {
                return Err("Original-coordinate confirmation cancelled before saving.".to_owned());
            }
            has_input |= frame.has_recorded_annotation_input();
            match frame.capture_binding {
                CaptureBinding::ArchivedAfterComposite=>return Err("A mixed-source composite cannot be confirmed as original capture pixels. Unselect it, undo the composite, or use manual annotations.".to_owned()),
                CaptureBinding::NotRecorded=>return Err("An imported or generated frame has no original screen-input coordinate source. Unselect it or use manual annotations; it cannot be confirmed as a recording.".to_owned()),
                CaptureBinding::Original=>{},
                CaptureBinding::LegacyUnknown=>{
                    changes.push(FrameCaptureBindingChange{frame_id:frame.id,binding:CaptureBinding::Original});
                }
            }
        }
        if !has_input {
            return Err("The selection contains no recorded input to confirm.".to_owned());
        }
        if changes.is_empty() {
            return Err(
                "No selected legacy frames need original-coordinate confirmation.".to_owned(),
            );
        }
        let count = changes.len();
        if cancellation.load(Ordering::Acquire) {
            return Err("Original-coordinate confirmation cancelled before saving.".to_owned());
        }
        self.execute(EditCommand::SetCaptureBindings { changes })
            .map_err(|error| error.to_string())?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_application::{
        IncrementalRecordingProject, IncrementalRecordingProjectOptions,
    };
    use gif_from_screen_domain::{FrameId, KeyStroke, PhysicalSize, ProjectId, TimeUs, UnixTimeMs};
    use gif_from_screen_gif::RgbaFrame;
    use gif_from_screen_project::LockPolicy;

    #[test]
    fn imported_or_generated_frames_cannot_be_claimed_as_original_even_without_raw_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("not-recorded"));
        let mut synthetic = workspace.manifest().timeline.frames[0].clone();
        synthetic.id = FrameId::from_u128(99);
        synthetic.capture_binding = CaptureBinding::NotRecorded;
        synthetic.capture_metadata = gif_from_screen_domain::CaptureMetadata::default();
        workspace
            .execute(EditCommand::InsertFrames {
                index: 1,
                frames: vec![synthetic],
            })
            .unwrap();
        workspace.select_all();
        let before = workspace.manifest().clone();
        let error = workspace
            .confirm_original_capture_binding(
                &workspace.project_edit_anchor(),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(error.contains("no original screen-input"));
        assert_eq!(workspace.manifest(), &before);
    }

    fn workspace(root: &std::path::Path) -> EditorWorkspace {
        let mut writer = IncrementalRecordingProject::create(
            root,
            PhysicalSize::new(240, 40).unwrap(),
            IncrementalRecordingProjectOptions {
                project_id: ProjectId::from_u128(11),
                app_version: "binding-test".to_owned(),
                created_at: UnixTimeMs::new(0),
                source_label: None,
            },
        )
        .unwrap();
        writer
            .append_frame(
                FrameId::from_u128(1),
                &RgbaFrame::new(240, 40, vec![255; 240 * 40 * 4], 10_000).unwrap(),
            )
            .unwrap();
        let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 16).unwrap();
        workspace.select_first().unwrap();
        let mut replacement = workspace.manifest().timeline.frames[0].clone();
        replacement.capture_binding = CaptureBinding::LegacyUnknown;
        replacement.capture_metadata.key_strokes.push(KeyStroke {
            physical_key: "C".to_owned(),
            display_text: Some("Ctrl+C".to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 2,
        });
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: replacement.id,
                replacement: Box::new(replacement),
            })
            .unwrap();
        workspace
    }

    #[test]
    fn confirmation_changes_only_binding_and_survives_undo_redo_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("legacy.gfsproj");
        let mut workspace = workspace(&root);
        let original = workspace.manifest().timeline.frames[0].clone();
        let revision = workspace.manifest().revision;
        assert_eq!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    &AtomicBool::new(false)
                )
                .unwrap(),
            1
        );
        let mut expected = original.clone();
        expected.capture_binding = CaptureBinding::Original;
        assert_eq!(workspace.manifest().timeline.frames[0], expected);
        assert_eq!(workspace.manifest().revision.get(), revision.get() + 1);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline.frames[0], original);
        workspace.redo().unwrap();
        assert_eq!(workspace.manifest().timeline.frames[0], expected);
        drop(workspace);
        let reopened = EditorWorkspace::open(root, LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().timeline.frames[0], expected);
    }

    #[test]
    fn confirming_a_legacy_interval_includes_empty_frames_so_key_hold_can_cross_them() {
        use gif_from_screen_domain::{
            AnnotationMode, AnnotationRequest, CaptureMetadata, OverlayContent,
            capture_binding_summary,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("interval.gfsproj"));
        let mut second = workspace.manifest().timeline.frames[0].clone();
        second.id = FrameId::from_u128(2);
        second.capture_metadata = CaptureMetadata::default();
        workspace
            .execute(EditCommand::InsertFrames {
                index: 1,
                frames: vec![second.clone()],
            })
            .unwrap();
        workspace.select_all();
        let summary = capture_binding_summary(&workspace.manifest().timeline.frames);
        assert_eq!(summary.legacy_unknown, 1);
        assert_eq!(summary.selected_legacy_unknown, 2);
        assert_eq!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    &AtomicBool::new(false)
                )
                .unwrap(),
            2
        );
        assert!(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.capture_binding == CaptureBinding::Original)
        );
        assert_eq!(
            workspace.manifest().timeline.frames[1].capture_metadata,
            second.capture_metadata
        );
        let prepared = crate::annotation_engine::prepare_annotations_with_assets(
            workspace.manifest(),
            workspace.selection().selected(),
            &AnnotationRequest {
                mode: AnnotationMode::RecordedKeys,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|_| panic!("key label preparation does not read source assets"),
        )
        .unwrap();
        let Some(EditCommand::UpsertOverlayTrack { track }) = prepared.commands.last() else {
            panic!("expected prepared key track")
        };
        assert_eq!(track.items.len(), 2);
        assert!(track.items.iter().all(
            |item| matches!(&item.content,OverlayContent::KeyStroke{text,..}if text=="Ctrl+C")
        ));
        workspace.undo().unwrap();
        assert!(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.capture_binding == CaptureBinding::LegacyUnknown)
        );
        workspace
            .execute(EditCommand::SetCaptureBindings {
                changes: vec![FrameCaptureBindingChange {
                    frame_id: second.id,
                    binding: CaptureBinding::ArchivedAfterComposite,
                }],
            })
            .unwrap();
        let before = workspace.manifest().clone();
        assert!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .contains("mixed-source")
        );
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn input_empty_legacy_images_do_not_create_meaningless_confirmation_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("empty.gfsproj"));
        let mut frame = workspace.manifest().timeline.frames[0].clone();
        frame.capture_metadata = gif_from_screen_domain::CaptureMetadata::default();
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            })
            .unwrap();
        let before = workspace.manifest().clone();
        assert!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .contains("no recorded input")
        );
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn archive_or_stale_confirmation_leaves_the_project_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("archive.gfsproj"));
        let stale = workspace.project_edit_anchor();
        let before_cancel = workspace.manifest().clone();
        assert!(
            workspace
                .confirm_original_capture_binding(&stale, &AtomicBool::new(true))
                .unwrap_err()
                .contains("cancelled")
        );
        assert_eq!(workspace.manifest(), &before_cancel);
        let mut replacement = workspace.manifest().timeline.frames[0].clone();
        replacement.capture_binding = CaptureBinding::ArchivedAfterComposite;
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: replacement.id,
                replacement: Box::new(replacement),
            })
            .unwrap();
        let before = workspace.manifest().clone();
        assert!(
            workspace
                .confirm_original_capture_binding(&stale, &AtomicBool::new(false))
                .unwrap_err()
                .contains("changed")
        );
        assert!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .contains("mixed-source")
        );
        assert_eq!(workspace.manifest(), &before);
    }
}
