//! Explicit user confirmation for legacy input associations; composited frames cannot be relabeled.

use gif_from_screen_domain::{CaptureBinding, EditCommand, FrameCaptureBindingChange};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{EditorWorkspace, OverlaySelectionAnchor};

impl EditorWorkspace {
    /// Confirm coordinates only unless the user separately declares a shared original clock.
    pub(crate) fn confirm_original_capture_binding(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        declare_common_clock: bool,
        cancellation: &AtomicBool,
    ) -> Result<usize, String> {
        let (command, count) =
            self.capture_confirmation_command(anchor, declare_common_clock, cancellation)?;
        check_cancelled(cancellation)?;
        self.execute(command).map_err(|error| error.to_string())?;
        Ok(count)
    }

    fn capture_confirmation_command(
        &self,
        anchor: &OverlaySelectionAnchor,
        declare_common_clock: bool,
        cancellation: &AtomicBool,
    ) -> Result<(EditCommand, usize), String> {
        check_cancelled(cancellation)?;
        if !anchor.matches(self) {
            return Err(
                "The project or selection changed before confirming original capture coordinates."
                    .to_owned(),
            );
        }
        if self.selection().is_empty() {
            return Err("Select the source frames whose original pixels you verified.".to_owned());
        }
        if self.selection().selected().len() > 100_000 {
            return Err("Confirm at most 100,000 original frames at a time.".to_owned());
        }
        let mut bindings = Vec::new();
        let mut clocks = Vec::new();
        let mut changed = std::collections::BTreeSet::new();
        let mut used: std::collections::BTreeSet<_> = self
            .manifest()
            .timeline
            .frames
            .iter()
            .filter_map(|frame| frame.capture_clock.and_then(|clock| clock.id))
            .collect();
        let mut run_identity = None;
        let mut source_time = 0_u64;
        let mut has_input = false;
        for frame in &self.manifest().timeline.frames {
            check_cancelled(cancellation)?;
            let sample_time = gif_from_screen_domain::TimeUs::new(source_time);
            source_time = source_time
                .checked_add(frame.duration.get())
                .ok_or("Source timeline duration overflow.")?;
            if !self.selection().contains(frame.id) {
                run_identity = None;
                continue;
            }
            has_input |= frame.has_recorded_annotation_input();
            match frame.capture_binding {
                CaptureBinding::ArchivedAfterComposite=>return Err("A mixed-source composite cannot be confirmed as original capture pixels. Unselect it, undo the composite, or use manual annotations.".to_owned()),
                CaptureBinding::NotRecorded=>return Err("An imported or generated frame has no original screen-input coordinate source. Unselect it or use manual annotations; it cannot be confirmed as a recording.".to_owned()),
                CaptureBinding::Original=>{},
                CaptureBinding::LegacyUnknown=>{bindings.push(FrameCaptureBindingChange{frame_id:frame.id,binding:CaptureBinding::Original});changed.insert(frame.id);},
            }
            let mut clock =
                frame
                    .capture_clock
                    .unwrap_or(gif_from_screen_domain::CaptureClockContext {
                        id: None,
                        sampled_at: frame.capture_metadata.captured_at.unwrap_or(sample_time),
                    });
            if clock.id.is_some() {
                run_identity = None;
            } else if declare_common_clock {
                let id = *run_identity.get_or_insert_with(|| fresh_clock_id(&mut used));
                clock.id = Some(id);
            }
            if frame.capture_clock != Some(clock) {
                clocks.push(gif_from_screen_domain::FrameCaptureClockChange {
                    frame_id: frame.id,
                    clock: Some(clock),
                });
                changed.insert(frame.id);
            }
        }
        if !has_input {
            return Err("The selection contains no recorded input to confirm.".to_owned());
        }
        if bindings.is_empty() && !declare_common_clock {
            return Err("Coordinates are already confirmed. Declare the shared recording clock separately to enable cross-frame input hold.".to_owned());
        }
        if changed.is_empty() {
            return Err("No selected source association needs confirmation.".to_owned());
        }
        let mut commands = Vec::new();
        if !bindings.is_empty() {
            commands.push(EditCommand::SetCaptureBindings { changes: bindings });
        }
        if !clocks.is_empty() {
            commands.push(EditCommand::SetCaptureClocks { changes: clocks });
        }
        Ok((EditCommand::Compound { commands }, changed.len()))
    }
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Original-coordinate confirmation cancelled before saving.".to_owned())
    } else {
        Ok(())
    }
}

fn fresh_clock_id(
    used: &mut std::collections::BTreeSet<gif_from_screen_domain::CaptureClockId>,
) -> gif_from_screen_domain::CaptureClockId {
    loop {
        let id = gif_from_screen_domain::CaptureClockId::from_u128(uuid::Uuid::new_v4().as_u128());
        if used.insert(id) {
            return id;
        }
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
    fn coordinate_confirmation_does_not_imply_common_clock_and_later_declaration_is_undoable() {
        use gif_from_screen_domain::{AnnotationMode, AnnotationRequest, CaptureMetadata};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("separate-declarations");
        let mut workspace = workspace(&root);
        let mut second = workspace.manifest().timeline.frames[0].clone();
        second.id = FrameId::from_u128(2);
        second.capture_metadata = CaptureMetadata::default();
        workspace
            .execute(EditCommand::InsertFrames {
                index: 1,
                frames: vec![second],
            })
            .unwrap();
        workspace.select_all();
        workspace
            .confirm_original_capture_binding(
                &workspace.project_edit_anchor(),
                false,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.capture_clock.unwrap().id.is_none())
        );
        let request = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        };
        let prepare = |workspace: &EditorWorkspace| {
            crate::annotation_engine::prepare_annotations_with_assets(
                workspace.manifest(),
                workspace.selection().selected(),
                &request,
                &AtomicBool::new(false),
                |_| {},
                &|_| panic!("keys do not load source pixels"),
            )
            .unwrap()
        };
        assert_eq!(prepare(&workspace).frames, 1);
        let before = workspace.manifest().clone();
        workspace
            .confirm_original_capture_binding(
                &workspace.project_edit_anchor(),
                true,
                &AtomicBool::new(false),
            )
            .unwrap();
        let frames = &workspace.manifest().timeline.frames;
        assert_eq!(
            frames[0].capture_clock.unwrap().id,
            frames[1].capture_clock.unwrap().id
        );
        assert!(frames[0].capture_clock.unwrap().id.is_some());
        assert_eq!(prepare(&workspace).frames, 2);
        workspace.undo().unwrap();
        let mut undone = workspace.manifest().clone();
        undone.revision = before.revision;
        assert_eq!(undone, before);
        workspace.redo().unwrap();
        drop(workspace);
        let mut reopened = EditorWorkspace::open(root, LockPolicy::FailIfPresent, 16).unwrap();
        reopened.select_all();
        assert_eq!(prepare(&reopened).frames, 2);
    }

    #[test]
    fn clock_declaration_never_relabels_known_clocks_or_joins_unknown_intervals_across_them() {
        use gif_from_screen_domain::{
            CaptureClockContext, CaptureClockId, FrameCaptureClockChange,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&dir.path().join("known-clocks"));
        let first = workspace.manifest().timeline.frames[0].clone();
        let extra = (2..=6)
            .map(|id| {
                let mut frame = first.clone();
                frame.id = FrameId::from_u128(id);
                frame.capture_metadata = gif_from_screen_domain::CaptureMetadata::default();
                frame
            })
            .collect();
        workspace
            .execute(EditCommand::InsertFrames {
                index: 1,
                frames: extra,
            })
            .unwrap();
        let a = CaptureClockId::from_u128(1);
        let b = CaptureClockId::from_u128(2);
        let at = |frame_id, id, micros| FrameCaptureClockChange {
            frame_id,
            clock: Some(CaptureClockContext {
                id: Some(id),
                sampled_at: TimeUs::new(micros),
            }),
        };
        workspace
            .execute(EditCommand::SetCaptureClocks {
                changes: vec![
                    at(first.id, a, 0),
                    at(FrameId::from_u128(3), b, 20_000),
                    at(FrameId::from_u128(6), a, 50_000),
                ],
            })
            .unwrap();
        workspace.select_all();
        let original = workspace.manifest().clone();
        let (command, _) = workspace
            .capture_confirmation_command(
                &workspace.project_edit_anchor(),
                true,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(!serde_json::to_string(&command).unwrap().contains("Ctrl+C"));
        workspace
            .confirm_original_capture_binding(
                &workspace.project_edit_anchor(),
                true,
                &AtomicBool::new(false),
            )
            .unwrap();
        let clocks: Vec<_> = workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| frame.capture_clock.unwrap())
            .collect();
        assert_eq!(clocks[0].id, Some(a));
        assert_eq!(clocks[2].id, Some(b));
        assert_eq!(clocks[5].id, Some(a));
        assert_eq!(clocks[3].id, clocks[4].id);
        assert_ne!(clocks[1].id, clocks[3].id);
        for clock in [clocks[1], clocks[3]] {
            assert!(clock.id.is_some());
            assert_ne!(clock.id, Some(a));
            assert_ne!(clock.id, Some(b));
        }
        assert_eq!(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| &frame.capture_metadata)
                .collect::<Vec<_>>(),
            original
                .timeline
                .frames
                .iter()
                .map(|frame| &frame.capture_metadata)
                .collect::<Vec<_>>()
        );
        workspace.undo().unwrap();
        let mut restored = workspace.manifest().clone();
        restored.revision = original.revision;
        assert_eq!(restored, original);
    }

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
                false,
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
    fn confirmation_changes_only_association_fields_and_survives_undo_redo_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("legacy.gfsproj");
        let mut workspace = workspace(&root);
        let original = workspace.manifest().timeline.frames[0].clone();
        let revision = workspace.manifest().revision;
        assert_eq!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    false,
                    &AtomicBool::new(false)
                )
                .unwrap(),
            1
        );
        let mut expected = original.clone();
        expected.capture_binding = CaptureBinding::Original;
        expected.freeze_capture_clock(TimeUs::ZERO);
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
                    true,
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
        assert_eq!(track.frame_cells.as_ref().unwrap().len(), 2);
        assert_eq!(track.all_mark_contents().count(), 2);
        assert!(track.all_mark_contents().all(
            |(_, content)| matches!(content,OverlayContent::KeyStroke{text,..}if text=="Ctrl+C")
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
                    false,
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
                    false,
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
                .confirm_original_capture_binding(&stale, false, &AtomicBool::new(true))
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
                .confirm_original_capture_binding(&stale, false, &AtomicBool::new(false))
                .unwrap_err()
                .contains("changed")
        );
        assert!(
            workspace
                .confirm_original_capture_binding(
                    &workspace.project_edit_anchor(),
                    false,
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .contains("mixed-source")
        );
        assert_eq!(workspace.manifest(), &before);
    }
}
