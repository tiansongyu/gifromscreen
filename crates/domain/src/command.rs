use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    AssetDescriptor, AssetId, Canvas, DomainError, DurationUs, FrameClip, FrameId, GifExportPreset,
    OverlayTrack, ProjectManifest, ProjectRevision, TrackId, Transition,
};

/// A frame and its original stable timeline position, used by inverse delete
/// commands. The index is a view position; identity remains `frame.id`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedFrame {
    pub index: usize,
    pub frame: FrameClip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameDurationChange {
    pub frame_id: FrameId,
    pub duration: DurationUs,
}

/// Every user-visible edit is a serializable value. Asset bytes are never part
/// of a command; commands refer to immutable content-addressed assets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditCommand {
    RegisterAsset {
        asset: AssetDescriptor,
    },
    UnregisterAsset {
        asset_id: AssetId,
    },
    InsertFrames {
        index: usize,
        frames: Vec<FrameClip>,
    },
    RemoveFrames {
        frame_ids: Vec<FrameId>,
    },
    RestoreFrames {
        frames: Vec<IndexedFrame>,
    },
    /// Exact inverse of a frame edit that changed overlay timing. Keeping the
    /// original spans avoids cumulative rounding during repeated undo/redo.
    /// `edit` must be a single insert, remove, restore, replace, or duration edit.
    RestoreFrameEdit {
        edit: Box<EditCommand>,
        overlay_tracks: Vec<OverlayTrack>,
    },
    ReplaceFrame {
        frame_id: FrameId,
        replacement: Box<FrameClip>,
    },
    SetFrameDurations {
        changes: Vec<FrameDurationChange>,
    },
    /// Changes only source-input associations in one bounded identity list.
    SetCaptureBindings {
        changes: Vec<crate::FrameCaptureBindingChange>,
    },
    ReorderFrames {
        order: Vec<FrameId>,
    },
    SetCanvas {
        canvas: Canvas,
    },
    UpsertOverlayTrack {
        track: OverlayTrack,
    },
    RemoveOverlayTrack {
        track_id: TrackId,
    },
    RestoreOverlayTrack {
        index: usize,
        track: OverlayTrack,
    },
    SetTransitions {
        transitions: Vec<Transition>,
    },
    UpsertExportPreset {
        name: String,
        preset: GifExportPreset,
    },
    RemoveExportPreset {
        name: String,
    },
    SetTaskRuns {
        runs: Vec<crate::EditTaskRun>,
    },
    /// Commands in a compound edit are one revision and are atomic. Inverses
    /// are stored in reverse order.
    Compound {
        commands: Vec<EditCommand>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedEdit {
    pub from_revision: ProjectRevision,
    pub to_revision: ProjectRevision,
    pub inverse: EditCommand,
}

impl ProjectManifest {
    /// Applies one atomic edit and advances the revision exactly once.
    ///
    /// Validation failure leaves the project byte-for-byte unchanged. The
    /// returned inverse is itself serializable and may be committed as a normal
    /// edit to implement persistent undo.
    pub fn apply_command(&mut self, command: &EditCommand) -> Result<AppliedEdit, DomainError> {
        self.validate()?;
        let before = self.clone();
        let from_revision = self.revision;
        let to_revision = from_revision.next().ok_or(DomainError::RevisionOverflow)?;

        let result = command.apply_inner(self).and_then(|inverse| {
            self.revision = to_revision;
            self.validate()?;
            Ok(inverse)
        });

        match result {
            Ok(inverse) => Ok(AppliedEdit {
                from_revision,
                to_revision,
                inverse,
            }),
            Err(error) => {
                *self = before;
                Err(error)
            }
        }
    }
}

impl EditCommand {
    fn apply_inner(&self, project: &mut ProjectManifest) -> Result<Self, DomainError> {
        if let Self::RestoreFrameEdit {
            edit,
            overlay_tracks,
        } = self
        {
            if !edit.changes_frame_timing() {
                return Err(DomainError::InvalidFrameEditRestore);
            }
            let inverse = edit.apply_without_retiming(project)?;
            let previous =
                std::mem::replace(&mut project.timeline.overlay_tracks, overlay_tracks.clone());
            return Ok(Self::RestoreFrameEdit {
                edit: Box::new(inverse),
                overlay_tracks: previous,
            });
        }

        if !self.changes_frame_timing() || project.timeline.overlay_tracks.is_empty() {
            return self.apply_without_retiming(project);
        }
        let unchanged_duration = match self {
            Self::ReplaceFrame {
                frame_id,
                replacement,
            } => project
                .timeline
                .frames
                .iter()
                .find(|frame| frame.id == *frame_id)
                .is_some_and(|frame| frame.duration == replacement.duration),
            _ => false,
        };
        if unchanged_duration {
            return self.apply_without_retiming(project);
        }

        let before: Vec<_> = project
            .timeline
            .frames
            .iter()
            .map(|frame| (frame.id, frame.duration))
            .collect();
        let inverse = self.apply_without_retiming(project)?;
        let timing = crate::overlay_timing::FrameTimingMap::new(&before, &project.timeline.frames)?;
        let previous = project.timeline.overlay_tracks.clone();
        timing.retime(&mut project.timeline.overlay_tracks)?;
        if project.timeline.overlay_tracks == previous {
            Ok(inverse)
        } else {
            Ok(Self::RestoreFrameEdit {
                edit: Box::new(inverse),
                overlay_tracks: previous,
            })
        }
    }

    const fn changes_frame_timing(&self) -> bool {
        matches!(
            self,
            Self::InsertFrames { .. }
                | Self::RemoveFrames { .. }
                | Self::RestoreFrames { .. }
                | Self::ReplaceFrame { .. }
                | Self::SetFrameDurations { .. }
        )
    }

    fn apply_without_retiming(&self, project: &mut ProjectManifest) -> Result<Self, DomainError> {
        match self {
            Self::RestoreFrameEdit { .. } => self.apply_inner(project),
            Self::RegisterAsset { asset } => {
                if project.assets.contains_key(&asset.id) {
                    return Err(DomainError::DuplicateAssetId(asset.id));
                }
                project.assets.insert(asset.id, asset.clone());
                Ok(Self::UnregisterAsset { asset_id: asset.id })
            }
            Self::UnregisterAsset { asset_id } => {
                if project.references_asset(*asset_id) {
                    return Err(DomainError::AssetStillReferenced(*asset_id));
                }
                let asset = project
                    .assets
                    .remove(asset_id)
                    .ok_or(DomainError::UnknownAsset(*asset_id))?;
                Ok(Self::RegisterAsset { asset })
            }
            Self::InsertFrames { index, frames } => {
                let len = project.timeline.frames.len();
                if *index > len {
                    return Err(DomainError::FrameIndexOutOfBounds { index: *index, len });
                }
                ensure_unique_frame_command(frames.iter().map(|frame| frame.id))?;
                let existing: BTreeSet<_> = project
                    .timeline
                    .frames
                    .iter()
                    .map(|frame| frame.id)
                    .collect();
                if let Some(frame) = frames.iter().find(|frame| existing.contains(&frame.id)) {
                    return Err(DomainError::DuplicateFrameId(frame.id));
                }
                project
                    .timeline
                    .frames
                    .splice(*index..*index, frames.iter().cloned());
                Ok(Self::RemoveFrames {
                    frame_ids: frames.iter().map(|frame| frame.id).collect(),
                })
            }
            Self::RemoveFrames { frame_ids } => {
                ensure_unique_frame_command(frame_ids.iter().copied())?;
                let requested: BTreeSet<_> = frame_ids.iter().copied().collect();
                let positions: BTreeMap<_, _> = project
                    .timeline
                    .frames
                    .iter()
                    .enumerate()
                    .map(|(index, frame)| (frame.id, index))
                    .collect();
                if let Some(frame_id) = frame_ids.iter().find(|id| !positions.contains_key(id)) {
                    return Err(DomainError::UnknownFrame(*frame_id));
                }
                let removed = project
                    .timeline
                    .frames
                    .iter()
                    .enumerate()
                    .filter(|(_, frame)| requested.contains(&frame.id))
                    .map(|(index, frame)| IndexedFrame {
                        index,
                        frame: frame.clone(),
                    })
                    .collect();
                project
                    .timeline
                    .frames
                    .retain(|frame| !requested.contains(&frame.id));
                Ok(Self::RestoreFrames { frames: removed })
            }
            Self::RestoreFrames { frames } => {
                ensure_unique_frame_command(frames.iter().map(|entry| entry.frame.id))?;
                let existing: BTreeSet<_> = project
                    .timeline
                    .frames
                    .iter()
                    .map(|frame| frame.id)
                    .collect();
                if let Some(entry) = frames
                    .iter()
                    .find(|entry| existing.contains(&entry.frame.id))
                {
                    return Err(DomainError::DuplicateFrameId(entry.frame.id));
                }
                let mut ordered = frames.clone();
                ordered.sort_by_key(|entry| entry.index);
                for entry in &ordered {
                    let len = project.timeline.frames.len();
                    if entry.index > len {
                        return Err(DomainError::RestoreIndexOutOfBounds {
                            index: entry.index,
                            len,
                        });
                    }
                    project
                        .timeline
                        .frames
                        .insert(entry.index, entry.frame.clone());
                }
                Ok(Self::RemoveFrames {
                    frame_ids: ordered.iter().map(|entry| entry.frame.id).collect(),
                })
            }
            Self::SetCaptureBindings { changes } => {
                ensure_unique_frame_command(changes.iter().map(|change| change.frame_id))?;
                let requested: BTreeMap<_, _> = changes
                    .iter()
                    .map(|change| (change.frame_id, change.binding))
                    .collect();
                let existing: BTreeSet<_> = project
                    .timeline
                    .frames
                    .iter()
                    .map(|frame| frame.id)
                    .collect();
                if let Some(change) = changes
                    .iter()
                    .find(|change| !existing.contains(&change.frame_id))
                {
                    return Err(DomainError::UnknownFrame(change.frame_id));
                }
                let mut inverse = Vec::with_capacity(changes.len());
                for frame in &mut project.timeline.frames {
                    if let Some(binding) = requested.get(&frame.id) {
                        inverse.push(crate::FrameCaptureBindingChange {
                            frame_id: frame.id,
                            binding: frame.capture_binding,
                        });
                        frame.capture_binding = *binding;
                    }
                }
                Ok(Self::SetCaptureBindings { changes: inverse })
            }
            Self::ReplaceFrame {
                frame_id,
                replacement,
            } => {
                if replacement.id != *frame_id {
                    return Err(DomainError::FrameIdentityMismatch {
                        expected: *frame_id,
                        actual: replacement.id,
                    });
                }
                let frame = project
                    .timeline
                    .frames
                    .iter_mut()
                    .find(|frame| frame.id == *frame_id)
                    .ok_or(DomainError::UnknownFrame(*frame_id))?;
                let previous = std::mem::replace(frame, replacement.as_ref().clone());
                Ok(Self::ReplaceFrame {
                    frame_id: *frame_id,
                    replacement: Box::new(previous),
                })
            }
            Self::SetFrameDurations { changes } => {
                ensure_unique_frame_command(changes.iter().map(|change| change.frame_id))?;
                for change in changes {
                    if !project
                        .timeline
                        .frames
                        .iter()
                        .any(|frame| frame.id == change.frame_id)
                    {
                        return Err(DomainError::UnknownFrame(change.frame_id));
                    }
                }
                let requested: BTreeMap<_, _> = changes
                    .iter()
                    .map(|change| (change.frame_id, change.duration))
                    .collect();
                let mut inverse = Vec::with_capacity(changes.len());
                for frame in &mut project.timeline.frames {
                    if let Some(duration) = requested.get(&frame.id) {
                        inverse.push(FrameDurationChange {
                            frame_id: frame.id,
                            duration: frame.duration,
                        });
                        frame.duration = *duration;
                    }
                }
                Ok(Self::SetFrameDurations { changes: inverse })
            }
            Self::ReorderFrames { order } => {
                ensure_unique_frame_command(order.iter().copied())?;
                let current_order: Vec<_> = project
                    .timeline
                    .frames
                    .iter()
                    .map(|frame| frame.id)
                    .collect();
                let current_set: BTreeSet<_> = current_order.iter().copied().collect();
                let requested_set: BTreeSet<_> = order.iter().copied().collect();
                if order.len() != current_order.len() || requested_set != current_set {
                    return Err(DomainError::ReorderDoesNotMatchTimeline);
                }
                let mut frames: BTreeMap<_, _> = std::mem::take(&mut project.timeline.frames)
                    .into_iter()
                    .map(|frame| (frame.id, frame))
                    .collect();
                project.timeline.frames = order
                    .iter()
                    .map(|id| frames.remove(id).expect("set equality checked above"))
                    .collect();
                Ok(Self::ReorderFrames {
                    order: current_order,
                })
            }
            Self::SetCanvas { canvas } => {
                let previous = std::mem::replace(&mut project.canvas, canvas.clone());
                Ok(Self::SetCanvas { canvas: previous })
            }
            Self::UpsertOverlayTrack { track } => {
                if let Some(existing) = project
                    .timeline
                    .overlay_tracks
                    .iter_mut()
                    .find(|existing| existing.id == track.id)
                {
                    let previous = std::mem::replace(existing, track.clone());
                    Ok(Self::UpsertOverlayTrack { track: previous })
                } else {
                    project.timeline.overlay_tracks.push(track.clone());
                    Ok(Self::RemoveOverlayTrack { track_id: track.id })
                }
            }
            Self::RemoveOverlayTrack { track_id } => {
                let index = project
                    .timeline
                    .overlay_tracks
                    .iter()
                    .position(|track| track.id == *track_id)
                    .ok_or(DomainError::UnknownTrack(*track_id))?;
                let track = project.timeline.overlay_tracks.remove(index);
                Ok(Self::RestoreOverlayTrack { index, track })
            }
            Self::RestoreOverlayTrack { index, track } => {
                let len = project.timeline.overlay_tracks.len();
                if *index > len {
                    return Err(DomainError::TrackIndexOutOfBounds { index: *index, len });
                }
                if project
                    .timeline
                    .overlay_tracks
                    .iter()
                    .any(|existing| existing.id == track.id)
                {
                    return Err(DomainError::DuplicateTrackId(track.id));
                }
                project
                    .timeline
                    .overlay_tracks
                    .insert(*index, track.clone());
                Ok(Self::RemoveOverlayTrack { track_id: track.id })
            }
            Self::SetTransitions { transitions } => {
                let previous =
                    std::mem::replace(&mut project.timeline.transitions, transitions.clone());
                Ok(Self::SetTransitions {
                    transitions: previous,
                })
            }
            Self::UpsertExportPreset { name, preset } => {
                match project.export_presets.insert(name.clone(), preset.clone()) {
                    Some(previous) => Ok(Self::UpsertExportPreset {
                        name: name.clone(),
                        preset: previous,
                    }),
                    None => Ok(Self::RemoveExportPreset { name: name.clone() }),
                }
            }
            Self::RemoveExportPreset { name } => {
                let preset = project
                    .export_presets
                    .remove(name)
                    .ok_or_else(|| DomainError::UnknownExportPreset(name.clone()))?;
                Ok(Self::UpsertExportPreset {
                    name: name.clone(),
                    preset,
                })
            }
            Self::SetTaskRuns { runs } => Ok(Self::SetTaskRuns {
                runs: std::mem::replace(&mut project.task_runs, runs.clone()),
            }),
            Self::Compound { commands } => {
                if commands.is_empty() {
                    return Err(DomainError::EmptyCommand);
                }
                let mut inverses = Vec::with_capacity(commands.len());
                for command in commands {
                    inverses.push(command.apply_inner(project)?);
                }
                inverses.reverse();
                Ok(Self::Compound { commands: inverses })
            }
        }
    }
}

fn ensure_unique_frame_command(
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for frame_id in frame_ids {
        if !seen.insert(frame_id) {
            return Err(DomainError::DuplicateFrameInCommand(frame_id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        AssetId,
        model::test_fixtures::{asset, frame, manifest},
    };

    use super::*;

    #[test]
    fn binding_edits_preserve_input_and_reject_duplicate_or_unknown_ids_atomically() {
        use crate::{CaptureBinding, FrameCaptureBindingChange, KeyStroke, TimeUs};
        let mut project = manifest();
        let asset = asset(1);
        project.assets.insert(asset.id, asset.clone());
        let mut first = frame(1, asset.id);
        first.capture_binding = CaptureBinding::LegacyUnknown;
        first.capture_metadata.key_strokes.push(KeyStroke {
            physical_key: "PRIVATE_RETAINED_INPUT".to_owned(),
            display_text: None,
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 0,
        });
        project.timeline.frames = vec![first.clone(), frame(2, asset.id)];
        let original = project.clone();
        let change = FrameCaptureBindingChange {
            frame_id: first.id,
            binding: CaptureBinding::Original,
        };
        for changes in [
            vec![change, change],
            vec![FrameCaptureBindingChange {
                frame_id: crate::FrameId::from_u128(99),
                binding: CaptureBinding::Original,
            }],
        ] {
            assert!(
                project
                    .apply_command(&EditCommand::SetCaptureBindings { changes })
                    .is_err()
            );
            assert_eq!(project, original);
        }
        let command = EditCommand::SetCaptureBindings {
            changes: vec![change],
        };
        let applied = project.apply_command(&command).unwrap();
        assert_eq!(
            project.timeline.frames[0].capture_metadata,
            first.capture_metadata
        );
        for edit in [&command, &applied.inverse] {
            assert!(
                !serde_json::to_string(edit)
                    .unwrap()
                    .contains("PRIVATE_RETAINED_INPUT")
            );
        }
        project.apply_command(&applied.inverse).unwrap();
        project.revision = original.revision;
        assert_eq!(project, original);
    }

    #[test]
    fn bulk_binding_edit_handles_one_hundred_thousand_frames_without_nested_commands() {
        use crate::{CaptureBinding, FrameCaptureBindingChange};
        let mut project = manifest();
        let asset = asset(1);
        project.assets.insert(asset.id, asset.clone());
        project.timeline.frames = (1..=100_000_u128)
            .map(|id| FrameClip {
                id: FrameId::from_u128(id),
                capture_binding: CaptureBinding::LegacyUnknown,
                ..frame(1, asset.id)
            })
            .collect();
        let changes = project
            .timeline
            .frames
            .iter()
            .map(|frame| FrameCaptureBindingChange {
                frame_id: frame.id,
                binding: CaptureBinding::Original,
            })
            .collect();
        let command = EditCommand::SetCaptureBindings { changes };
        assert!(serde_json::to_vec(&command).unwrap().len() < 16 * 1024 * 1024);
        let inverse = project.apply_command(&command).unwrap().inverse;
        assert!(
            project
                .timeline
                .frames
                .iter()
                .all(|frame| frame.capture_binding == CaptureBinding::Original)
        );
        assert!(
            matches!(inverse,EditCommand::SetCaptureBindings{ref changes} if changes.len()==100_000)
        );
        project.apply_command(&inverse).unwrap();
        assert!(
            project
                .timeline
                .frames
                .iter()
                .all(|frame| frame.capture_binding == CaptureBinding::LegacyUnknown)
        );
    }

    #[test]
    fn compound_asset_and_frame_insert_is_atomic_and_invertible() {
        let mut project = manifest();
        let original = project.clone();
        let asset = asset(1);
        let command = EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: asset.clone(),
                },
                EditCommand::InsertFrames {
                    index: 0,
                    frames: vec![frame(1, asset.id), frame(2, asset.id)],
                },
            ],
        };

        let applied = project.apply_command(&command).unwrap();
        assert_eq!(applied.from_revision, ProjectRevision::ZERO);
        assert_eq!(applied.to_revision, ProjectRevision::new(1));
        assert_eq!(project.timeline.frames.len(), 2);

        project.apply_command(&applied.inverse).unwrap();
        let revision_after_undo = project.revision;
        project.revision = original.revision;
        assert_eq!(project, original);
        assert_eq!(revision_after_undo, ProjectRevision::new(2));
    }

    #[test]
    fn failed_compound_rolls_back_every_mutation() {
        let mut project = manifest();
        let before = project.clone();
        let asset = asset(1);
        let bad_frame = frame(1, AssetId::from_digest([99; 32]));
        let command = EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset { asset },
                EditCommand::InsertFrames {
                    index: 0,
                    frames: vec![bad_frame],
                },
            ],
        };

        assert!(project.apply_command(&command).is_err());
        assert_eq!(project, before);
    }

    #[test]
    fn inverse_restores_sparse_removals_at_original_positions() {
        let mut project = manifest();
        let asset = asset(1);
        project
            .apply_command(&EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: asset.clone(),
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: (1..=5).map(|number| frame(number, asset.id)).collect(),
                    },
                ],
            })
            .unwrap();
        let expected: Vec<_> = project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect();
        let applied = project
            .apply_command(&EditCommand::RemoveFrames {
                frame_ids: vec![FrameId::from_u128(2), FrameId::from_u128(4)],
            })
            .unwrap();
        project.apply_command(&applied.inverse).unwrap();
        let actual: Vec<_> = project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn serialized_command_round_trips() {
        let command = EditCommand::SetFrameDurations {
            changes: vec![FrameDurationChange {
                frame_id: FrameId::from_u128(7),
                duration: DurationUs::new(42_000).unwrap(),
            }],
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<EditCommand>(&json).unwrap(), command);
    }

    #[test]
    fn many_reorders_preserve_identity_and_total_duration() {
        let mut project = manifest();
        let asset = asset(1);
        project
            .apply_command(&EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: asset.clone(),
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: (1..=32).map(|number| frame(number, asset.id)).collect(),
                    },
                ],
            })
            .unwrap();
        let total = project.timeline.total_duration();
        for shift in 1..32 {
            let mut order: Vec<_> = project
                .timeline
                .frames
                .iter()
                .map(|frame| frame.id)
                .collect();
            order.rotate_left(shift % 32);
            project
                .apply_command(&EditCommand::ReorderFrames { order })
                .unwrap();
            assert_eq!(project.timeline.total_duration(), total);
            project.validate().unwrap();
        }
    }
}
