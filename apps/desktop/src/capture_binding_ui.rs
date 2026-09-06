//! Explicit, selection-scoped confirmation for legacy recorded input.

use eframe::egui;
use gif_from_screen_domain::{CaptureBindingSummary, capture_binding_summary};

use crate::editor_workspace::{EditorWorkspace, OverlaySelectionAnchor};

#[derive(Default)]
pub(crate) struct CaptureBindingUi {
    anchor: Option<OverlaySelectionAnchor>,
    counts: CaptureBindingSummary,
    verified_original_pixels: bool,
    verified_common_clock: bool,
}

impl CaptureBindingUi {
    /// The root queues the returned request in the existing annotation worker.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, workspace: &EditorWorkspace) -> Option<bool> {
        let mut confirm = None;
        egui::CollapsingHeader::new("Recorded input provenance")
            .id_salt("recorded-input-provenance")
            .show(ui, |ui| {
                self.sync_workspace(workspace);
                ui.label(format!(
                    "Selected frames with saved input: {} original · {} unverified legacy · {} baked · {} without a recording association",
                    self.counts.original, self.counts.legacy_unknown, self.counts.archived_after_composite, self.counts.not_recorded
                ));
                if self.counts.selected_archived_after_composite > 0 {
                    ui.label("Baked frames retain their original recorded events for recovery, but those events are not automatically replayed onto mixed-source pixels. Undo the bake or use manual annotations.");
                }
                if self.counts.selected_not_recorded > 0 {
                    ui.label("Some selected frames were created without screen-input metadata, such as titles or imported images. They cannot be relabeled as screen recordings; unselect them before confirming a legacy recording interval.");
                }
                if !self.needs_confirmation() || !self.has_recorded_input() {
                    ui.weak("No selected input association needs confirmation. Pixels, saved annotations and GIF export remain unchanged.");
                    return;
                }
                self.show_acknowledgements(ui);
                if self.counts.selected_archived_after_composite > 0 {
                    ui.weak("Unselect the baked frames before confirming legacy frames. Known baked frames cannot be relabeled as original.");
                }
                if ui
                    .add_enabled(self.can_confirm(), egui::Button::new("Confirm selected input association"))
                    .clicked()
                {
                    confirm = self.take_confirmation();
                }
                ui.weak("Raw events, pixels and existing annotations remain intact. One Undo restores the prior associations; reapply recorded annotations to use the confirmed information.");
            });
        confirm
    }

    fn show_acknowledgements(&mut self, ui: &mut egui::Ui) {
        if self.counts.selected_legacy_unknown > 0 {
            ui.label("Earlier projects did not record whether a frame had been baked. Only confirm after checking that the selected legacy frames still use their original captured pixels, with any crop or resize kept as editable transforms.");
            ui.label(format!("Pixel confirmation includes all {} selected legacy frames, including frames between recorded events. It does not restore pixels or remap coordinates.", self.counts.selected_legacy_unknown));
            ui.checkbox(
                &mut self.verified_original_pixels,
                "I verified that these legacy frames retain their original captured pixels",
            );
        }
        if self.counts.selected_missing_clock > 0 {
            ui.label(format!("{} selected frames have no verified recording clock. Pixel confirmation alone does not allow key or click holds to continue between them.", self.counts.selected_missing_clock));
            ui.checkbox(
                &mut self.verified_common_clock,
                "Each selected continuous unknown-clock interval is from one recording with trustworthy original sample times",
            );
            ui.weak("Optional and separate from pixel confirmation. Known recording clocks are never merged. Leave this unchecked if the original timing is uncertain; use per-frame or manual annotations instead.");
        }
    }

    fn sync_workspace(&mut self, workspace: &EditorWorkspace) {
        if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| anchor.matches(workspace))
        {
            return;
        }
        self.verified_original_pixels = false;
        self.verified_common_clock = false;
        self.counts = capture_binding_summary(
            workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .filter(|frame| workspace.selection().contains(frame.id)),
        );
        self.anchor = Some(workspace.project_edit_anchor());
    }

    fn can_confirm(&self) -> bool {
        self.has_recorded_input()
            && (self.counts.selected_legacy_unknown == 0 || self.verified_original_pixels)
            && (self.counts.selected_legacy_unknown > 0
                || self.counts.selected_missing_clock > 0 && self.verified_common_clock)
            && self.counts.selected_archived_after_composite == 0
            && self.counts.selected_not_recorded == 0
    }

    fn needs_confirmation(&self) -> bool {
        self.counts.selected_legacy_unknown > 0 || self.counts.selected_missing_clock > 0
    }

    fn has_recorded_input(&self) -> bool {
        self.counts.original
            + self.counts.legacy_unknown
            + self.counts.archived_after_composite
            + self.counts.not_recorded
            > 0
    }

    fn take_confirmation(&mut self) -> Option<bool> {
        let confirmed = self.can_confirm().then_some(self.verified_common_clock);
        self.verified_original_pixels = false;
        self.verified_common_clock = false;
        confirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_application::{
        IncrementalRecordingProject, IncrementalRecordingProjectOptions,
    };
    use gif_from_screen_domain::{
        CaptureBinding, EditCommand, FrameId, KeyStroke, PhysicalSize, ProjectId, TimeUs,
        UnixTimeMs,
    };
    use gif_from_screen_gif::RgbaFrame;

    fn workspace(root: &std::path::Path) -> EditorWorkspace {
        let mut writer = IncrementalRecordingProject::create(
            root,
            PhysicalSize::new(1, 1).unwrap(),
            IncrementalRecordingProjectOptions {
                project_id: ProjectId::from_u128(901),
                app_version: "legacy-ui-test".to_owned(),
                created_at: UnixTimeMs::new(0),
                source_label: None,
            },
        )
        .unwrap();
        for id in 1..=3 {
            writer
                .append_frame(
                    FrameId::from_u128(id),
                    &RgbaFrame::new(1, 1, vec![255; 4], 10_000).unwrap(),
                )
                .unwrap();
        }
        let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 16).unwrap();
        let mut legacy = workspace.manifest().timeline.frames[0].clone();
        legacy.capture_binding = CaptureBinding::LegacyUnknown;
        legacy.capture_metadata.key_strokes.push(KeyStroke {
            physical_key: "C".to_owned(),
            display_text: Some("Ctrl+C".to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 2,
        });
        let mut archived = workspace.manifest().timeline.frames[1].clone();
        archived.capture_binding = CaptureBinding::ArchivedAfterComposite;
        archived.capture_metadata = legacy.capture_metadata.clone();
        workspace
            .execute(EditCommand::Compound {
                commands: vec![
                    EditCommand::ReplaceFrame {
                        frame_id: legacy.id,
                        replacement: Box::new(legacy),
                    },
                    EditCommand::ReplaceFrame {
                        frame_id: archived.id,
                        replacement: Box::new(archived),
                    },
                ],
            })
            .unwrap();
        workspace.select_first().unwrap();
        workspace
    }

    #[test]
    fn confirmation_is_explicit_one_shot_and_never_allows_baked_frames() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("source.gfsproj"));
        let mut ui = CaptureBindingUi::default();
        ui.sync_workspace(&workspace);
        assert_eq!(ui.counts.legacy_unknown, 1);
        assert_eq!(ui.take_confirmation(), None);
        ui.verified_original_pixels = true;
        assert_eq!(ui.take_confirmation(), Some(false));
        assert_eq!(ui.take_confirmation(), None);
        workspace.select_all();
        ui.sync_workspace(&workspace);
        assert_eq!(ui.counts.archived_after_composite, 1);
        assert_eq!(ui.counts.legacy_unknown, 1);
        assert_eq!(
            ui.counts.original, 0,
            "empty image input is not a confirmation candidate"
        );
        ui.verified_original_pixels = true;
        assert_eq!(ui.take_confirmation(), None);
    }

    #[test]
    fn clock_confirmation_is_independent_and_can_follow_an_earlier_pixel_confirmation() {
        let mut ui = CaptureBindingUi {
            counts: CaptureBindingSummary {
                legacy_unknown: 1,
                selected_legacy_unknown: 2,
                selected_missing_clock: 2,
                ..CaptureBindingSummary::default()
            },
            verified_common_clock: true,
            ..CaptureBindingUi::default()
        };
        assert_eq!(
            ui.take_confirmation(),
            None,
            "clock trust cannot imply pixel trust"
        );
        ui.verified_original_pixels = true;
        assert_eq!(
            ui.take_confirmation(),
            Some(false),
            "pixel trust cannot imply clock trust"
        );
        ui.verified_original_pixels = true;
        ui.verified_common_clock = true;
        assert_eq!(ui.take_confirmation(), Some(true));
        assert_eq!(ui.take_confirmation(), None);

        ui.counts.legacy_unknown = 0;
        ui.counts.selected_legacy_unknown = 0;
        ui.counts.original = 1;
        assert_eq!(ui.take_confirmation(), None);
        ui.verified_common_clock = true;
        assert_eq!(
            ui.take_confirmation(),
            Some(true),
            "already confirmed pixels do not need reconfirmation"
        );
        ui.counts.selected_missing_clock = 0;
        ui.verified_common_clock = true;
        assert_eq!(
            ui.take_confirmation(),
            None,
            "known clocks are not reassigned"
        );
    }

    #[test]
    fn empty_interval_frames_can_be_confirmed_but_empty_baked_frames_still_block() {
        let mut ui = CaptureBindingUi {
            counts: CaptureBindingSummary {
                original: 1,
                selected_legacy_unknown: 2,
                ..CaptureBindingSummary::default()
            },
            verified_original_pixels: true,
            ..CaptureBindingUi::default()
        };
        assert!(
            ui.can_confirm(),
            "original input can extend into verified empty legacy frames"
        );
        ui.counts.selected_archived_after_composite = 1;
        assert!(
            !ui.can_confirm(),
            "an empty baked frame is still a replay barrier"
        );
        ui.counts.selected_archived_after_composite = 0;
        ui.counts.selected_not_recorded = 1;
        assert!(
            !ui.can_confirm(),
            "non-recorded title or image frames cannot be confirmed as recordings"
        );
        ui.counts.selected_not_recorded = 0;
        ui.counts.original = 0;
        assert!(
            !ui.can_confirm(),
            "old images alone do not need input confirmation"
        );
    }

    #[test]
    fn acknowledgement_does_not_survive_a_selection_revision_or_project_change() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("source.gfsproj"));
        let mut ui = CaptureBindingUi::default();
        ui.sync_workspace(&workspace);
        ui.verified_original_pixels = true;
        ui.verified_common_clock = true;
        ui.sync_workspace(&workspace);
        assert!(ui.can_confirm());
        assert!(ui.verified_common_clock);
        workspace.select_all();
        ui.sync_workspace(&workspace);
        assert!(!ui.verified_original_pixels);
        assert!(!ui.verified_common_clock);
        workspace.select_first().unwrap();
        ui.sync_workspace(&workspace);
        ui.verified_original_pixels = true;
        ui.verified_common_clock = true;
        let original = workspace.manifest().timeline.frames[0].clone();
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: original.id,
                replacement: Box::new(original),
            })
            .unwrap();
        ui.sync_workspace(&workspace);
        assert!(!ui.verified_original_pixels);
        assert!(!ui.verified_common_clock);
        ui.verified_original_pixels = true;
        ui.verified_common_clock = true;
        let other = self::workspace(&directory.path().join("other.gfsproj"));
        ui.sync_workspace(&other);
        assert!(
            !ui.verified_original_pixels,
            "even matching project IDs cannot transfer consent across paths"
        );
        assert!(!ui.verified_common_clock);
    }
}
