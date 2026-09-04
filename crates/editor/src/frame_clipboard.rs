use std::collections::{BTreeMap, BTreeSet};

use gif_from_screen_domain::{EditCommand, FrameClip, FrameId, ProjectManifest};

use crate::{EditorError, ensure_known_selection, remove_frames_atomically};

/// Maximum number of clips retained by one application clipboard snapshot.
pub const MAX_FRAME_CLIPBOARD_FRAMES: usize = 512;

/// One bounded, in-memory snapshot of copied frame clips.
///
/// Clips retain their source identities inside the snapshot so Copy itself is lossless. Paste
/// always replaces those identities with newly generated [`FrameId`] values. Immutable asset
/// references, duration, transform, capture metadata, and effects are cloned unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameClipboard {
    frames: Vec<FrameClip>,
}

impl FrameClipboard {
    /// Returns the number of copied clips.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Returns whether this snapshot contains no clips.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Borrows copied clips in original timeline order.
    pub fn frames(&self) -> &[FrameClip] {
        &self.frames
    }
}

/// Atomic cut result: the clipboard is installed only after `command` commits successfully.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CutFrameSelection {
    /// Lossless source clips in timeline order.
    pub clipboard: FrameClipboard,
    /// One transition-safe removal command for persistent commit.
    pub command: EditCommand,
}

/// Copies a bounded selection in timeline order without changing the project.
///
/// Input order and duplicate identities are ignored.
///
/// # Errors
///
/// Returns an error for empty/unknown selection or more than
/// [`MAX_FRAME_CLIPBOARD_FRAMES`] selected clips.
pub fn copy_selected_frames(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<FrameClipboard, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    if selected.len() > MAX_FRAME_CLIPBOARD_FRAMES {
        return Err(EditorError::FrameClipboardTooLarge {
            selected: selected.len(),
            maximum: MAX_FRAME_CLIPBOARD_FRAMES,
        });
    }
    Ok(FrameClipboard {
        frames: project
            .timeline
            .frames
            .iter()
            .filter(|frame| selected.contains(&frame.id))
            .cloned()
            .collect(),
    })
}

/// Builds one atomic cut command together with the clipboard to install after commit.
///
/// Cutting every timeline frame is rejected so Cut cannot leave an unusable empty project. Copying
/// and removal are resolved from the same validated selection.
///
/// # Errors
///
/// Returns the Copy errors, or [`EditorError::CutWouldEmptyTimeline`] when all frames are selected.
pub fn cut_selected_frames(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<CutFrameSelection, EditorError> {
    let clipboard = copy_selected_frames(project, frame_ids)?;
    if clipboard.len() == project.timeline.frames.len() {
        return Err(EditorError::CutWouldEmptyTimeline {
            frame_count: project.timeline.frames.len(),
        });
    }
    let removed = clipboard.frames.iter().map(|frame| frame.id).collect();
    Ok(CutFrameSelection {
        clipboard,
        command: remove_frames_atomically(project, removed),
    })
}

/// Builds one atomic paste command after `current`, or at timeline end when `current` is `None`.
///
/// Every pasted clip receives a fresh identity from `generate_frame_id`. Asset references and all
/// other clip fields are reused unchanged. A transition crossing the insertion point is removed;
/// no transition is synthesized for pasted frames.
///
/// # Errors
///
/// Returns an error for an empty clipboard, stale current frame, unavailable/incompatible source
/// asset, nil/conflicting generated identity, or timeline-duration overflow.
pub fn paste_frame_clipboard<G>(
    project: &ProjectManifest,
    clipboard: &FrameClipboard,
    current: Option<FrameId>,
    mut generate_frame_id: G,
) -> Result<EditCommand, EditorError>
where
    G: FnMut() -> FrameId,
{
    if clipboard.is_empty() {
        return Err(EditorError::EmptyFrameClipboard);
    }
    let insertion_index = match current {
        Some(frame_id) => project
            .timeline
            .frames
            .iter()
            .position(|frame| frame.id == frame_id)
            .and_then(|index| index.checked_add(1))
            .ok_or(EditorError::UnknownPasteAnchor(frame_id))?,
        None => project.timeline.frames.len(),
    };
    validate_clipboard_assets(project, clipboard)?;

    let mut occupied: BTreeSet<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let mut pasted = Vec::with_capacity(clipboard.len());
    let mut added_duration = 0_u64;
    for source in &clipboard.frames {
        let frame_id = generate_frame_id();
        if frame_id.is_nil() {
            return Err(EditorError::GeneratedNilFrameId);
        }
        if !occupied.insert(frame_id) {
            return Err(EditorError::GeneratedFrameIdConflict(frame_id));
        }
        added_duration = added_duration
            .checked_add(source.duration.get())
            .ok_or(EditorError::InvalidDuration)?;
        let mut clone = source.clone();
        clone.id = frame_id;
        pasted.push(clone);
    }
    project
        .timeline
        .total_duration()
        .and_then(|duration| duration.get().checked_add(added_duration))
        .ok_or(EditorError::InvalidDuration)?;

    let retained_transitions = transitions_after_insertion(project, insertion_index, &pasted);
    let mut commands = Vec::with_capacity(2);
    if retained_transitions.len() != project.timeline.transitions.len() {
        commands.push(EditCommand::SetTransitions {
            transitions: retained_transitions,
        });
    }
    commands.push(EditCommand::InsertFrames {
        index: insertion_index,
        frames: pasted,
    });
    Ok(EditCommand::Compound { commands })
}

fn validate_clipboard_assets(
    project: &ProjectManifest,
    clipboard: &FrameClipboard,
) -> Result<(), EditorError> {
    for frame in &clipboard.frames {
        let asset = project
            .assets
            .get(&frame.asset_id)
            .ok_or(EditorError::MissingFrameAsset {
                frame_id: frame.id,
                asset_id: frame.asset_id,
            })?;
        if !asset.kind.is_frame() {
            return Err(EditorError::UnsupportedFrameAsset {
                frame_id: frame.id,
                asset_id: frame.asset_id,
            });
        }
    }
    Ok(())
}

fn transitions_after_insertion(
    project: &ProjectManifest,
    insertion_index: usize,
    pasted: &[FrameClip],
) -> Vec<gif_from_screen_domain::Transition> {
    let mut order = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect::<Vec<_>>();
    order.splice(
        insertion_index..insertion_index,
        pasted.iter().map(|frame| frame.id),
    );
    let positions: BTreeMap<_, _> = order
        .into_iter()
        .enumerate()
        .map(|(index, frame_id)| (frame_id, index))
        .collect();
    project
        .timeline
        .transitions
        .iter()
        .filter(|transition| {
            positions
                .get(&transition.from_frame)
                .zip(positions.get(&transition.to_frame))
                .is_some_and(|(from, to)| from.checked_add(1) == Some(*to))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, DurationUs, Effect, PhysicalSize, ProjectId, ProjectRevision,
        RasterEncoding, Rgba, Timeline, Transition, TransitionId, TransitionKind, UnixTimeMs,
    };

    use super::*;

    fn project(frame_count: usize) -> ProjectManifest {
        let size = PhysicalSize::new(2, 2).unwrap();
        let asset_id = AssetId::from_digest([7; 32]);
        let mut assets = BTreeMap::new();
        assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 16,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let frames = (0..frame_count)
            .map(|index| FrameClip {
                id: FrameId::from_u128(u128::try_from(index).unwrap() + 1),
                asset_id,
                duration: DurationUs::new(u64::try_from(index).unwrap() + 1).unwrap(),
                transform: ClipTransform {
                    flip_horizontal: index % 2 == 0,
                    ..ClipTransform::default()
                },
                capture_metadata: CaptureMetadata {
                    dropped_frames_before: u32::try_from(index).unwrap(),
                    ..CaptureMetadata::default()
                },
                effects: vec![Effect::Border {
                    widths: gif_from_screen_domain::EdgeWidths {
                        top: 1,
                        right: 0,
                        bottom: 0,
                        left: 0,
                    },
                    color: Rgba {
                        red: u8::try_from(index % 251).unwrap(),
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                }],
            })
            .collect::<Vec<_>>();
        let transitions = frames
            .windows(2)
            .enumerate()
            .map(|(index, pair)| Transition {
                id: TransitionId::from_u128(u128::try_from(index).unwrap() + 1),
                from_frame: pair[0].id,
                to_frame: pair[1].id,
                duration: DurationUs::new(1).unwrap(),
                steps: 1,
                kind: TransitionKind::FadeToNext,
            })
            .collect();
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "clipboard-test".to_owned(),
            created_at: UnixTimeMs::new(1),
            canvas: Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
            timeline: Timeline {
                frames,
                transitions,
                ..Timeline::default()
            },
            assets,
            export_presets: BTreeMap::new(),
            source_provenance: Vec::new(),
        }
    }

    fn ids(frames: &[FrameClip]) -> Vec<FrameId> {
        frames.iter().map(|frame| frame.id).collect()
    }

    #[test]
    fn copy_is_bounded_deduplicated_and_follows_timeline_order() {
        let timeline_project = project(4);
        let clipboard = copy_selected_frames(
            &timeline_project,
            [
                FrameId::from_u128(3),
                FrameId::from_u128(1),
                FrameId::from_u128(3),
            ],
        )
        .unwrap();
        assert_eq!(
            ids(clipboard.frames()),
            [FrameId::from_u128(1), FrameId::from_u128(3)]
        );
        assert_eq!(clipboard.frames()[0], timeline_project.timeline.frames[0]);

        let oversized = project(MAX_FRAME_CLIPBOARD_FRAMES + 1);
        assert!(matches!(
            copy_selected_frames(
                &oversized,
                oversized.timeline.frames.iter().map(|frame| frame.id)
            ),
            Err(EditorError::FrameClipboardTooLarge { .. })
        ));
    }

    #[test]
    fn cut_is_one_transition_safe_command_and_rejects_entire_timeline() {
        let mut timeline_project = project(4);
        let cut = cut_selected_frames(
            &timeline_project,
            [FrameId::from_u128(2), FrameId::from_u128(3)],
        )
        .unwrap();
        assert_eq!(
            ids(cut.clipboard.frames()),
            [FrameId::from_u128(2), FrameId::from_u128(3)]
        );
        let before = timeline_project.timeline.clone();
        let inverse = timeline_project
            .apply_command(&cut.command)
            .unwrap()
            .inverse;
        assert_eq!(
            ids(&timeline_project.timeline.frames),
            [FrameId::from_u128(1), FrameId::from_u128(4)]
        );
        assert!(timeline_project.timeline.transitions.is_empty());
        timeline_project.apply_command(&inverse).unwrap();
        assert_eq!(timeline_project.timeline.frames, before.frames);
        assert_eq!(timeline_project.timeline.transitions, before.transitions);

        let all = project(2);
        assert!(matches!(
            cut_selected_frames(&all, all.timeline.frames.iter().map(|frame| frame.id)),
            Err(EditorError::CutWouldEmptyTimeline { frame_count: 2 })
        ));
    }

    #[test]
    fn paste_after_current_and_append_without_current_preserve_clip_state() {
        let source = project(4);
        let clipboard =
            copy_selected_frames(&source, [FrameId::from_u128(2), FrameId::from_u128(3)]).unwrap();
        let mut generated = [FrameId::from_u128(10), FrameId::from_u128(11)].into_iter();
        let command =
            paste_frame_clipboard(&source, &clipboard, Some(FrameId::from_u128(1)), || {
                generated.next().unwrap()
            })
            .unwrap();
        let mut pasted = source.clone();
        let inverse = pasted.apply_command(&command).unwrap().inverse;
        assert_eq!(
            ids(&pasted.timeline.frames),
            [
                FrameId::from_u128(1),
                FrameId::from_u128(10),
                FrameId::from_u128(11),
                FrameId::from_u128(2),
                FrameId::from_u128(3),
                FrameId::from_u128(4),
            ]
        );
        assert_eq!(
            pasted.timeline.frames[1].asset_id,
            clipboard.frames()[0].asset_id
        );
        assert_eq!(
            pasted.timeline.frames[1].effects,
            clipboard.frames()[0].effects
        );
        assert_eq!(
            pasted.timeline.frames[1].transform,
            clipboard.frames()[0].transform
        );
        assert_eq!(
            pasted.timeline.frames[1].capture_metadata,
            clipboard.frames()[0].capture_metadata
        );
        pasted.apply_command(&inverse).unwrap();
        assert_eq!(pasted.timeline, source.timeline);

        let mut append_ids = [FrameId::from_u128(20), FrameId::from_u128(21)].into_iter();
        let append =
            paste_frame_clipboard(&source, &clipboard, None, || append_ids.next().unwrap())
                .unwrap();
        let EditCommand::Compound { commands } = append else {
            panic!("expected compound paste");
        };
        assert!(matches!(
            commands.last(),
            Some(EditCommand::InsertFrames { index: 4, .. })
        ));
    }

    #[test]
    fn paste_rejects_nil_existing_repeated_ids_and_stale_anchor() {
        let project = project(3);
        let clipboard =
            copy_selected_frames(&project, [FrameId::from_u128(1), FrameId::from_u128(2)]).unwrap();
        assert!(matches!(
            paste_frame_clipboard(&project, &clipboard, None, || FrameId::NIL),
            Err(EditorError::GeneratedNilFrameId)
        ));
        assert!(matches!(
            paste_frame_clipboard(&project, &clipboard, None, || FrameId::from_u128(1)),
            Err(EditorError::GeneratedFrameIdConflict(id)) if id == FrameId::from_u128(1)
        ));
        assert!(matches!(
            paste_frame_clipboard(&project, &clipboard, None, || FrameId::from_u128(9)),
            Err(EditorError::GeneratedFrameIdConflict(id)) if id == FrameId::from_u128(9)
        ));
        assert!(matches!(
            paste_frame_clipboard(
                &project,
                &clipboard,
                Some(FrameId::from_u128(99)),
                || FrameId::from_u128(10),
            ),
            Err(EditorError::UnknownPasteAnchor(id)) if id == FrameId::from_u128(99)
        ));
    }
}
