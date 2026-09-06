use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gif_from_screen_domain::{EditCommand, FrameClip, FrameId, ProjectManifest};
use thiserror::Error;

use crate::{
    EditorError, FrameBundle, FrameBundleIdentities, ensure_known_selection,
    remove_frames_atomically,
};

/// Maximum number of clips retained by one application clipboard snapshot.
pub const MAX_FRAME_CLIPBOARD_FRAMES: usize = 512;
/// Default number of Copy/Cut snapshots retained by an editor session.
pub const DEFAULT_FRAME_CLIPBOARD_HISTORY_CAPACITY: usize = 8;
/// Hard bound preventing an accidental UI setting from retaining an unbounded history.
pub const MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY: usize = 64;

/// One bounded, in-memory snapshot of copied frame clips.
///
/// Clips retain their source identities inside the snapshot so Copy itself is lossless. Paste
/// always replaces those identities with newly generated [`FrameId`] values. Immutable asset
/// references, duration, transform, capture metadata, and effects are cloned unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameClipboard {
    frames: Vec<FrameClip>,
    bundle: FrameBundle,
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

    /// Frozen frame-owned marks accompanying these clips; legacy timed tracks
    /// are intentionally not part of the clipboard.
    pub const fn frame_bundle(&self) -> &FrameBundle {
        &self.bundle
    }
}

/// Session-local stable identity for one clipboard-history snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FrameClipboardEntryId(u64);

impl FrameClipboardEntryId {
    /// Returns the monotonically assigned non-zero identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One selectable clipboard-history entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameClipboardHistoryEntry {
    id: FrameClipboardEntryId,
    clipboard: FrameClipboard,
}

impl FrameClipboardHistoryEntry {
    /// Returns this entry's stable session identity.
    pub const fn id(&self) -> FrameClipboardEntryId {
        self.id
    }

    /// Borrows the lossless frame snapshot used by Paste.
    pub const fn clipboard(&self) -> &FrameClipboard {
        &self.clipboard
    }

    /// Returns the number of frames available for a compact history preview.
    pub fn frame_count(&self) -> usize {
        self.clipboard.len()
    }

    /// Returns the exact summed duration when it fits `u64` microseconds.
    pub fn total_duration_us(&self) -> Option<u64> {
        self.clipboard
            .frames()
            .iter()
            .try_fold(0_u64, |total, frame| {
                total.checked_add(frame.duration.get())
            })
    }
}

/// Bounded Copy/Cut history with a stable selected entry.
///
/// Entries iterate from oldest to newest. Pushing selects the new entry and evicts exactly the
/// oldest entry at capacity. Selecting or deleting an entry never copies its frame payload. When
/// the selected entry is deleted, the newest survivor becomes current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameClipboardHistory {
    entries: VecDeque<FrameClipboardHistoryEntry>,
    capacity: usize,
    selected: Option<FrameClipboardEntryId>,
    next_id: u64,
}

impl Default for FrameClipboardHistory {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: DEFAULT_FRAME_CLIPBOARD_HISTORY_CAPACITY,
            selected: None,
            next_id: 1,
        }
    }
}

impl FrameClipboardHistory {
    /// Creates an empty history with a caller-selected bounded capacity.
    ///
    /// # Errors
    ///
    /// Returns an error for zero capacity or a value above
    /// [`MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY`].
    pub fn new(capacity: usize) -> Result<Self, FrameClipboardHistoryError> {
        if capacity == 0 {
            return Err(FrameClipboardHistoryError::ZeroCapacity);
        }
        if capacity > MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY {
            return Err(FrameClipboardHistoryError::CapacityTooLarge {
                requested: capacity,
                maximum: MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY,
            });
        }
        Ok(Self {
            entries: VecDeque::new(),
            capacity,
            selected: None,
            next_id: 1,
        })
    }

    /// Returns the configured entry bound.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of retained snapshots.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no clipboard snapshot is retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the selected history identity, if any.
    pub const fn selected_id(&self) -> Option<FrameClipboardEntryId> {
        self.selected
    }

    /// Iterates entries from oldest to newest.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &FrameClipboardHistoryEntry> {
        self.entries.iter()
    }

    /// Returns the currently selected entry.
    pub fn selected(&self) -> Option<&FrameClipboardHistoryEntry> {
        let selected = self.selected?;
        self.entries.iter().find(|entry| entry.id == selected)
    }

    /// Returns the currently selected snapshot used by Paste.
    pub fn selected_clipboard(&self) -> Option<&FrameClipboard> {
        self.selected().map(FrameClipboardHistoryEntry::clipboard)
    }

    /// Pushes and selects a snapshot, evicting the oldest entry only after all fallible checks.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty snapshot, exhausted session identities, or allocation
    /// failure. Existing history and selection remain unchanged on error.
    pub fn push(
        &mut self,
        clipboard: FrameClipboard,
    ) -> Result<FrameClipboardEntryId, FrameClipboardHistoryError> {
        self.prepare_push(&clipboard)?;
        Ok(self.insert_prepared(clipboard))
    }

    /// Runs a fallible operation and pushes the snapshot only when it succeeds.
    ///
    /// History validation and allocation happen before `operation` is invoked. The outer result
    /// reports a history preparation failure, while the inner result preserves the operation's
    /// own error type. An operation failure leaves entries and selection unchanged; an operation
    /// success is followed only by non-fallible ring-buffer mutation. This supports atomic Cut
    /// semantics when the durable project commit is supplied as `operation`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameClipboardHistoryError`] before invoking `operation` when the snapshot cannot
    /// be retained. The nested result returns the unchanged error produced by `operation`.
    pub fn push_after<T, E>(
        &mut self,
        clipboard: FrameClipboard,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<(FrameClipboardEntryId, T), E>, FrameClipboardHistoryError> {
        self.prepare_push(&clipboard)?;
        let value = match operation() {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let id = self.insert_prepared(clipboard);
        Ok(Ok((id, value)))
    }

    fn prepare_push(
        &mut self,
        clipboard: &FrameClipboard,
    ) -> Result<(), FrameClipboardHistoryError> {
        if clipboard.is_empty() {
            return Err(FrameClipboardHistoryError::EmptyClipboard);
        }
        if self.next_id == 0 {
            return Err(FrameClipboardHistoryError::IdentityExhausted);
        }
        if self.entries.len() < self.capacity {
            self.entries.try_reserve(1).map_err(|_| {
                FrameClipboardHistoryError::AllocationFailed {
                    requested: self.entries.len().saturating_add(1),
                }
            })?;
        }

        Ok(())
    }

    fn insert_prepared(&mut self, clipboard: FrameClipboard) -> FrameClipboardEntryId {
        debug_assert!(!clipboard.is_empty());
        debug_assert_ne!(self.next_id, 0);
        let id = FrameClipboardEntryId(self.next_id);
        let next_id = self.next_id.checked_add(1).unwrap_or(0);
        if self.entries.len() == self.capacity {
            let _ = self.entries.pop_front();
        }
        self.entries
            .push_back(FrameClipboardHistoryEntry { id, clipboard });
        self.selected = Some(id);
        self.next_id = next_id;
        id
    }

    /// Selects an existing entry without changing its position.
    ///
    /// Returns `false` and preserves the prior selection when `id` is stale or unknown.
    pub fn select(&mut self, id: FrameClipboardEntryId) -> bool {
        if self.entries.iter().any(|entry| entry.id == id) {
            self.selected = Some(id);
            true
        } else {
            false
        }
    }

    /// Removes one entry and returns its snapshot.
    ///
    /// Removing the selected entry selects the newest survivor. An unknown identity is a no-op.
    pub fn remove(&mut self, id: FrameClipboardEntryId) -> Option<FrameClipboard> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        let removed = self.entries.remove(index)?;
        if self.selected == Some(id) {
            self.selected = self.entries.back().map(FrameClipboardHistoryEntry::id);
        }
        Some(removed.clipboard)
    }

    /// Removes every entry while retaining capacity and monotonic identity state.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.selected = None;
    }
}

/// Invalid or unavailable clipboard-history operation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum FrameClipboardHistoryError {
    /// A useful history must retain at least one entry.
    #[error("frame clipboard history capacity must be greater than zero")]
    ZeroCapacity,
    /// The requested capacity exceeds the process-wide retention guardrail.
    #[error("frame clipboard history capacity {requested} exceeds the maximum of {maximum}")]
    CapacityTooLarge {
        /// Rejected number of retained entries.
        requested: usize,
        /// Process-wide maximum entry count.
        maximum: usize,
    },
    /// An externally constructed or future snapshot contains no frames.
    #[error("cannot add an empty snapshot to frame clipboard history")]
    EmptyClipboard,
    /// Reserving the bounded deque failed without changing history.
    #[error("could not allocate frame clipboard history for {requested} entries")]
    AllocationFailed {
        /// Entry count needed by the failed push.
        requested: usize,
    },
    /// The session-local monotonically increasing identity space is exhausted.
    #[error("frame clipboard history identity space is exhausted")]
    IdentityExhausted,
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
    let bundle = FrameBundle::capture(project, &selected)?;
    let mut frames = Vec::with_capacity(selected.len());
    let mut source_time = 0_u64;
    for frame in &project.timeline.frames {
        if selected.contains(&frame.id) {
            let mut copy = frame.clone();
            copy.freeze_capture_clock(gif_from_screen_domain::TimeUs::new(source_time));
            frames.push(copy);
        }
        source_time = source_time
            .checked_add(frame.duration.get())
            .ok_or(EditorError::InvalidDuration)?;
    }
    Ok(FrameClipboard { frames, bundle })
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
/// Frame-owned groups are copied without event recomputation. When present,
/// their fresh track/mark IDs consume additional values from the same injected
/// 128-bit identity source; legacy-only pastes retain the original call count.
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
    let mut frame_ids = BTreeMap::new();
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
        frame_ids.insert(source.id, frame_id);
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
    if !clipboard.bundle.tracks().is_empty() {
        let mut identities = FrameBundleIdentities::new(
            project
                .timeline
                .overlay_tracks
                .iter()
                .chain(clipboard.bundle.tracks()),
        )?;
        commands.extend(
            clipboard
                .bundle
                .remap(&frame_ids, &mut identities, &mut generate_frame_id)?
                .into_iter()
                .map(|track| EditCommand::UpsertOverlayTrack { track }),
        );
    }
    Ok(EditCommand::Compound { commands })
}

fn validate_clipboard_assets(
    project: &ProjectManifest,
    clipboard: &FrameClipboard,
) -> Result<(), EditorError> {
    clipboard.bundle.validate_assets(project)?;
    for frame in &clipboard.frames {
        let asset = project
            .assets
            .get(&frame.asset_id)
            .ok_or(EditorError::MissingFrameAsset {
                frame_id: frame.id,
                asset_id: frame.asset_id,
            })?;
        if asset.kind.raster_descriptor().is_none() {
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
                render_steps: Vec::new(),
                capture_clock: None,
                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
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
            task_runs: Vec::new(),
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
        let mut expected = timeline_project.timeline.frames[0].clone();
        expected.freeze_capture_clock(gif_from_screen_domain::TimeUs::ZERO);
        assert_eq!(clipboard.frames()[0], expected);
        assert_eq!(
            timeline_project.timeline.frames[0].capture_clock, None,
            "copy must not edit the source project"
        );

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
    fn paste_preserves_original_native_event_and_capture_clocks() {
        use gif_from_screen_domain::{KeyStroke, MouseButton, MouseInputEvent, TimeUs};
        let mut source = project(3);
        let metadata = &mut source.timeline.frames[1].capture_metadata;
        metadata.captured_at = Some(TimeUs::new(20_000));
        metadata.key_strokes.push(KeyStroke {
            physical_key: "x11:38".into(),
            display_text: Some("a".into()),
            pressed: true,
            at: TimeUs::new(19_000),
            repeat: false,
            modifiers: 0,
        });
        metadata.mouse_events.push(MouseInputEvent {
            at: TimeUs::new(19_500),
            button: MouseButton::Left,
            pressed: true,
            position: None,
        });
        let clipboard = copy_selected_frames(&source, [FrameId::from_u128(2)]).unwrap();
        let command =
            paste_frame_clipboard(&source, &clipboard, None, || FrameId::from_u128(9)).unwrap();
        source.apply_command(&command).unwrap();
        assert_eq!(
            source.timeline.frames[3].capture_metadata,
            source.timeline.frames[1].capture_metadata
        );
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

    #[test]
    fn clipboard_history_evicts_oldest_and_selects_each_new_snapshot() {
        let project = project(4);
        let first = copy_selected_frames(&project, [FrameId::from_u128(1)]).unwrap();
        let second = copy_selected_frames(&project, [FrameId::from_u128(2)]).unwrap();
        let third = copy_selected_frames(&project, [FrameId::from_u128(3)]).unwrap();
        let mut history = FrameClipboardHistory::new(2).unwrap();

        let first_id = history.push(first).unwrap();
        let second_id = history.push(second).unwrap();
        assert!(history.select(first_id));
        let third_id = history.push(third).unwrap();

        assert_eq!(history.capacity(), 2);
        assert_eq!(history.len(), 2);
        assert_eq!(history.selected_id(), Some(third_id));
        assert_eq!(
            history
                .entries()
                .map(FrameClipboardHistoryEntry::id)
                .collect::<Vec<_>>(),
            [second_id, third_id]
        );
        assert!(!history.select(first_id));
        assert_eq!(history.selected().unwrap().frame_count(), 1);
        assert_eq!(history.selected().unwrap().total_duration_us(), Some(3));
    }

    #[test]
    fn clipboard_history_selection_removal_and_clear_have_stable_fallbacks() {
        let project = project(4);
        let mut history = FrameClipboardHistory::default();
        let first = history
            .push(copy_selected_frames(&project, [FrameId::from_u128(1)]).unwrap())
            .unwrap();
        let middle = history
            .push(copy_selected_frames(&project, [FrameId::from_u128(2)]).unwrap())
            .unwrap();
        let newest = history
            .push(copy_selected_frames(&project, [FrameId::from_u128(3)]).unwrap())
            .unwrap();

        assert!(history.select(first));
        assert_eq!(
            history.remove(middle).unwrap().frames()[0].id,
            FrameId::from_u128(2)
        );
        assert_eq!(history.selected_id(), Some(first));
        assert_eq!(history.remove(first).unwrap().len(), 1);
        assert_eq!(history.selected_id(), Some(newest));
        assert!(history.remove(FrameClipboardEntryId(99)).is_none());

        history.clear();
        assert!(history.is_empty());
        assert!(history.selected_clipboard().is_none());
        let after_clear = history
            .push(copy_selected_frames(&project, [FrameId::from_u128(4)]).unwrap())
            .unwrap();
        assert_eq!(after_clear.get(), 4);
    }

    #[test]
    fn clipboard_history_rejects_invalid_capacity_empty_entries_and_exhausted_ids_atomically() {
        assert!(matches!(
            FrameClipboardHistory::new(0),
            Err(FrameClipboardHistoryError::ZeroCapacity)
        ));
        assert!(matches!(
            FrameClipboardHistory::new(MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY + 1),
            Err(FrameClipboardHistoryError::CapacityTooLarge { .. })
        ));

        let mut history = FrameClipboardHistory::new(1).unwrap();
        assert!(matches!(
            history.push(FrameClipboard {
                frames: Vec::new(),
                bundle: FrameBundle::default()
            }),
            Err(FrameClipboardHistoryError::EmptyClipboard)
        ));
        assert!(history.is_empty());

        let project = project(2);
        history.next_id = u64::MAX;
        let final_id = history
            .push(copy_selected_frames(&project, [FrameId::from_u128(1)]).unwrap())
            .unwrap();
        assert_eq!(final_id.get(), u64::MAX);
        let before = history.clone();
        assert!(matches!(
            history.push(copy_selected_frames(&project, [FrameId::from_u128(2)]).unwrap()),
            Err(FrameClipboardHistoryError::IdentityExhausted)
        ));
        assert_eq!(history, before);
    }

    #[test]
    fn clipboard_history_preview_reports_duration_overflow_without_panicking() {
        let mut clipboard = FrameClipboard {
            frames: project(2).timeline.frames,
            bundle: FrameBundle::default(),
        };
        clipboard.frames[0].duration = DurationUs::new(u64::MAX).unwrap();
        clipboard.frames[1].duration = DurationUs::new(1).unwrap();
        let mut history = FrameClipboardHistory::default();
        history.push(clipboard).unwrap();
        assert_eq!(history.selected().unwrap().total_duration_us(), None);
    }

    #[test]
    fn clipboard_history_push_after_is_atomic_around_the_supplied_operation() {
        let project = project(2);
        let first = copy_selected_frames(&project, [FrameId::from_u128(1)]).unwrap();
        let second = copy_selected_frames(&project, [FrameId::from_u128(2)]).unwrap();
        let mut history = FrameClipboardHistory::new(2).unwrap();
        history.push(first).unwrap();
        let before = history.clone();

        let failed = history
            .push_after(second.clone(), || Err::<(), _>("commit failed"))
            .unwrap();
        assert_eq!(failed, Err("commit failed"));
        assert_eq!(history, before);

        let succeeded = history
            .push_after(second, || Ok::<_, &str>(42_u8))
            .unwrap()
            .unwrap();
        assert_eq!(succeeded.1, 42);
        assert_eq!(history.selected_id(), Some(succeeded.0));
        assert_eq!(history.len(), 2);
    }
}
