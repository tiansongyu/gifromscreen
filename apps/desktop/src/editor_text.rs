//! Text authoring preserves editable attributes and freezes the pixels used for export.

use gif_from_screen_domain::{
    AssetId, AssetKind, BlendMode, CaptureMetadata, ClipTransform, DurationUs, EditCommand,
    FrameClip, FrameId, OverlayContent, OverlayId, OverlayItem, OverlayTrack, PhysicalPoint,
    PhysicalRect, PhysicalSize, RasterEncoding, Rgba, TextRaster, TimeUs, TimelineSpan, TrackId,
};
use gif_from_screen_text::{TextImage, TextRequest};
use uuid::Uuid;

use super::{EditorWorkspace, EditorWorkspaceError, RasterOverlayEdit};

#[derive(Clone, Debug)]
pub(crate) struct TextOverlayDraft {
    pub(crate) track_id: TrackId,
    pub(crate) request: TextRequest,
    pub(crate) position: PhysicalPoint,
}

#[derive(Clone, Debug)]
pub(crate) struct TitleFrameRequest {
    /// None inserts before the first frame; Some inserts after this stable identity.
    pub(crate) after: Option<FrameId>,
    pub(crate) duration: DurationUs,
    pub(crate) background: Rgba,
    pub(crate) text: TextRequest,
    pub(crate) position: PhysicalPoint,
}

impl EditorWorkspace {
    /// Reads one text group without changing its frame coverage or project state.
    pub(crate) fn text_overlay_draft(
        &self,
        track_id: TrackId,
    ) -> Result<TextOverlayDraft, EditorWorkspaceError> {
        let track = self.editable_text_track(track_id)?;
        let OverlayContent::Text {
            text,
            position,
            font_family,
            font_size_px,
            foreground,
            background,
            alignment,
            raster: Some(raster),
            ..
        } = &track.items[0].content
        else {
            return Err(EditorWorkspaceError::TextTrackNotEditable(track_id));
        };
        Ok(TextOverlayDraft {
            track_id,
            request: TextRequest {
                text: text.clone(),
                font_family: font_family.clone(),
                font_size_px: *font_size_px,
                size: raster.size,
                foreground: *foreground,
                background: *background,
                alignment: *alignment,
            },
            position: *position,
        })
    }

    /// Applies prepared text to its original group, independent of the current selection.
    /// Track settings, item identities, z order, and exact spans remain unchanged.
    pub(crate) fn replace_text_overlay(
        &mut self,
        track_id: TrackId,
        request: &TextRequest,
        image: &TextImage,
        position: PhysicalPoint,
    ) -> Result<TrackId, EditorWorkspaceError> {
        let mut track = self.editable_text_track(track_id)?.clone();
        self.validate_text_image(request, image, position)?;
        let asset = self.raster_asset_descriptor(image.size, &image.rgba)?;
        let content = text_content(request, image, position, asset.id);
        for item in &mut track.items {
            item.content.clone_from(&content);
        }
        self.commit_with_raster_assets(
            &[(asset, &image.rgba)],
            vec![EditCommand::UpsertOverlayTrack { track }],
        )?;
        Ok(track_id)
    }

    /// Persists shaped text pixels and their original editable attributes as one undoable edit.
    pub(crate) fn add_text_overlay_for_selection(
        &mut self,
        request: &TextRequest,
        image: &TextImage,
        position: PhysicalPoint,
    ) -> Result<TrackId, EditorWorkspaceError> {
        self.validate_text_image(request, image, position)?;
        self.add_raster_content_for_selection(
            RasterOverlayEdit {
                name: "Text".to_owned(),
                source_size: image.size,
                position,
                display_size: image.size,
                item_opacity: 255,
                track_opacity: 255,
                blend_mode: BlendMode::Normal,
                z_index: 3,
            },
            &image.rgba,
            |asset_id| text_content(request, image, position, asset_id),
        )
    }

    /// Inserts a standalone title without inheriting existing annotations at the insertion point.
    /// The frame, text, shifted tracks, and affected transition form one reversible revision.
    pub(crate) fn insert_title_frame(
        &mut self,
        request: &TitleFrameRequest,
        image: &TextImage,
    ) -> Result<FrameId, EditorWorkspaceError> {
        self.validate_text_image(&request.text, image, request.position)?;
        let frames = &self.manifest().timeline.frames;
        let index = match request.after {
            None => 0,
            Some(id) => {
                frames
                    .iter()
                    .position(|frame| frame.id == id)
                    .ok_or(EditorWorkspaceError::UnknownTitleAnchor(id))?
                    + 1
            }
        };
        let start = frames[..index]
            .iter()
            .try_fold(0_u64, |sum, frame| sum.checked_add(frame.duration.get()))
            .ok_or(EditorWorkspaceError::TitleDurationOverflow)?;
        self.manifest()
            .timeline
            .total_duration()
            .and_then(|duration| duration.get().checked_add(request.duration.get()))
            .ok_or(EditorWorkspaceError::TitleDurationOverflow)?;

        // A solid one-pixel source scales exactly to any canvas without allocating
        // a canvas-sized background buffer on the UI thread.
        let color = request.background;
        let background = [color.red, color.green, color.blue, color.alpha];
        let source_size = PhysicalSize::new(1, 1).expect("one pixel is nonempty");
        let mut frame_asset = self.raster_asset_descriptor(source_size, &background)?;
        if !self.manifest().assets.contains_key(&frame_asset.id) {
            frame_asset.kind = AssetKind::Frame {
                size: source_size,
                encoding: RasterEncoding::Rgba8,
            };
        }
        let text_asset = self.raster_asset_descriptor(image.size, &image.rgba)?;
        let frame_id = FrameId::from_u128(Uuid::new_v4().as_u128());
        let frame = FrameClip {
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::NotRecorded,
            id: frame_id,
            asset_id: frame_asset.id,
            duration: request.duration,
            transform: ClipTransform {
                output_size: Some(self.manifest().canvas.size),
                ..ClipTransform::default()
            },
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        };
        let mut commands = self.title_insertion_commands(index, start, request.duration, frame)?;
        commands.push(EditCommand::UpsertOverlayTrack {
            track: OverlayTrack {
                annotation: None,
                annotation_scope: None,
                id: TrackId::from_u128(Uuid::new_v4().as_u128()),
                name: "Title".to_owned(),
                visible: true,
                opacity: 255,
                blend_mode: BlendMode::Normal,
                items: vec![OverlayItem {
                    id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
                    span: TimelineSpan {
                        start: TimeUs::new(start),
                        duration: request.duration,
                    },
                    z_index: 3,
                    content: text_content(&request.text, image, request.position, text_asset.id),
                }],
            },
        });
        self.commit_with_raster_assets(
            &[(frame_asset, &background), (text_asset, &image.rgba)],
            commands,
        )?;
        self.select_only(frame_id)?;
        Ok(frame_id)
    }

    fn editable_text_track(
        &self,
        track_id: TrackId,
    ) -> Result<&OverlayTrack, EditorWorkspaceError> {
        let track = self
            .manifest()
            .timeline
            .overlay_tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or(EditorWorkspaceError::TextTrackNotEditable(track_id))?;
        let Some(first) = track.items.first() else {
            return Err(EditorWorkspaceError::TextTrackNotEditable(track_id));
        };
        if !matches!(
            first.content,
            OverlayContent::Text {
                raster: Some(_),
                ..
            }
        ) || track.items.iter().any(|item| item.content != first.content)
        {
            return Err(EditorWorkspaceError::TextTrackNotEditable(track_id));
        }
        Ok(track)
    }

    fn validate_text_image(
        &self,
        request: &TextRequest,
        image: &TextImage,
        position: PhysicalPoint,
    ) -> Result<(), EditorWorkspaceError> {
        request.validate()?;
        if request.size != image.size {
            return Err(EditorWorkspaceError::TextRasterDimensionsMismatch);
        }
        if !(PhysicalRect {
            origin: position,
            size: image.size,
        })
        .fits_within(self.manifest().canvas.size)
        {
            return Err(EditorWorkspaceError::RasterOverlayOutsideCanvas);
        }
        Ok(())
    }

    fn title_insertion_commands(
        &self,
        index: usize,
        start: u64,
        duration: DurationUs,
        frame: FrameClip,
    ) -> Result<Vec<EditCommand>, EditorWorkspaceError> {
        let timeline = &self.manifest().timeline;
        let preceding = index.checked_sub(1).map(|index| timeline.frames[index].id);
        let following = timeline.frames.get(index).map(|frame| frame.id);
        let transitions = timeline
            .transitions
            .iter()
            .filter(|transition| {
                Some(transition.from_frame) != preceding || Some(transition.to_frame) != following
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut commands = Vec::new();
        if transitions.len() != timeline.transitions.len() {
            commands.push(EditCommand::SetTransitions { transitions });
        }
        commands.push(EditCommand::InsertFrames {
            index,
            frames: vec![frame],
        });
        for track in &timeline.overlay_tracks {
            let mut shifted = track.clone();
            shifted.items = exclude_inserted_title(&track.items, start, duration)?;
            if let Some(scope) = &track.annotation_scope {
                shifted.annotation_scope = Some(
                    gif_from_screen_domain::shift_annotation_scope_for_insert(
                        scope, start, duration,
                    )
                    .map_err(|_| EditorWorkspaceError::TitleDurationOverflow)?,
                );
            }
            if shifted != *track {
                commands.push(EditCommand::UpsertOverlayTrack { track: shifted });
            }
        }
        Ok(commands)
    }
}

fn text_content(
    request: &TextRequest,
    image: &TextImage,
    position: PhysicalPoint,
    asset_id: AssetId,
) -> OverlayContent {
    OverlayContent::Text {
        text: request.text.clone(),
        position,
        max_width: Some(request.size.width),
        font_family: request.font_family.clone(),
        font_size_px: request.font_size_px,
        foreground: request.foreground,
        background: request.background,
        alignment: request.alignment,
        raster: Some(TextRaster {
            asset_id,
            size: image.size,
        }),
    }
}

pub(super) fn exclude_inserted_title(
    items: &[OverlayItem],
    start: u64,
    duration: DurationUs,
) -> Result<Vec<OverlayItem>, EditorWorkspaceError> {
    let mut shifted = Vec::with_capacity(items.len());
    for original in items {
        let end = original
            .span
            .end()
            .ok_or(EditorWorkspaceError::TitleDurationOverflow)?
            .get();
        let mut item = original.clone();
        if end <= start {
            shifted.push(item);
            continue;
        }
        if item.span.start.get() < start {
            item.span.duration = DurationUs::new(start - item.span.start.get())
                .ok_or(EditorWorkspaceError::TitleDurationOverflow)?;
            shifted.push(item);
            item = original.clone();
            item.id = OverlayId::from_u128(Uuid::new_v4().as_u128());
            item.span.start = TimeUs::new(start);
            item.span.duration =
                DurationUs::new(end - start).ok_or(EditorWorkspaceError::TitleDurationOverflow)?;
        }
        item.span.start = item
            .span
            .start
            .checked_add_duration(duration)
            .ok_or(EditorWorkspaceError::TitleDurationOverflow)?;
        shifted.push(item);
    }
    Ok(shifted)
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{
        HorizontalAlignment, ProjectManifest, ShapeKind, Transition, TransitionId, TransitionKind,
    };
    use gif_from_screen_project::LockPolicy;

    use super::super::tests::{create_rendered_duplicate_workspace, create_workspace, frame_id};
    use super::*;

    fn text(text: &str, color: Rgba) -> (TextRequest, TextImage) {
        let size = PhysicalSize::new(1, 1).unwrap();
        (
            TextRequest {
                text: text.to_owned(),
                font_family: "sans-serif".to_owned(),
                font_size_px: 12,
                size,
                foreground: color,
                background: None,
                alignment: HorizontalAlignment::Center,
            },
            TextImage {
                size,
                rgba: vec![color.red, color.green, color.blue, color.alpha],
            },
        )
    }

    const BLUE: Rgba = Rgba {
        red: 0,
        green: 0,
        blue: 255,
        alpha: 255,
    };
    const GREEN: Rgba = Rgba {
        red: 0,
        green: 255,
        blue: 0,
        alpha: 255,
    };
    const BLACK: Rgba = Rgba {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };

    fn assert_project_content(actual: &ProjectManifest, expected: &ProjectManifest) {
        let mut actual = actual.clone();
        actual.revision = expected.revision;
        assert_eq!(&actual, expected);
    }

    fn export_frames(
        workspace: &EditorWorkspace,
        name: &str,
    ) -> gif_from_screen_media::DecodedAnimation {
        let output = workspace.active_project().layout().root.join(name);
        gif_from_screen_application::export_project_snapshot_to_gif(
            &gif_from_screen_application::ProjectExportSnapshot::from_active(
                workspace.active_project(),
            ),
            &output,
            &gif_from_screen_application::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut gif_from_screen_application::NoopProjectExportProgress,
        )
        .unwrap();
        gif_from_screen_media::decode_gif(
            std::fs::File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn replacing_text_preserves_group_identity_spans_settings_and_exact_undo() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        let (original_request, original_image) = text("Before", GREEN);
        let track_id = workspace
            .add_text_overlay_for_selection(
                &original_request,
                &original_image,
                PhysicalPoint::default(),
            )
            .unwrap();
        let mut original_track = workspace.manifest().timeline.overlay_tracks[0].clone();
        original_track.name = "Selected captions".to_owned();
        original_track.items[0].z_index = -7;
        original_track.items[1].z_index = 9;
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: original_track.clone(),
            })
            .unwrap();
        let draft = workspace.text_overlay_draft(track_id).unwrap();
        assert_eq!(draft.track_id, track_id);
        assert_eq!(draft.request.text, original_request.text);
        assert_eq!(draft.request.font_family, original_request.font_family);
        assert_eq!(draft.request.alignment, original_request.alignment);
        assert_eq!(draft.request.size, original_image.size);
        assert_eq!(draft.position, PhysicalPoint::default());
        let original = workspace.manifest().clone();
        workspace.clear_selection();
        let (request, image) = text("After", BLUE);
        assert_eq!(
            workspace
                .replace_text_overlay(track_id, &request, &image, draft.position)
                .unwrap(),
            track_id
        );
        let edited = workspace.manifest().clone();
        let edited_track = &edited.timeline.overlay_tracks[0];
        let mut expected_track = original_track;
        for item in &mut expected_track.items {
            item.content = edited_track.items[0].content.clone();
        }
        assert_eq!(edited_track, &expected_track);
        for _ in 0..20 {
            assert!(workspace.undo().unwrap());
            assert_project_content(workspace.manifest(), &original);
            assert!(workspace.redo().unwrap());
            assert_project_content(workspace.manifest(), &edited);
        }
        drop(workspace);
        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_project_content(reopened.manifest(), &edited);
        let decoded = export_frames(&reopened, "edited-text.gif");
        assert_eq!(&decoded.frames()[0].rgba()[..4], image.rgba.as_slice());
        assert_eq!(&decoded.frames()[1].rgba()[..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded.frames()[2].rgba()[..4], image.rgba.as_slice());
    }

    #[test]
    fn title_insertion_at_beginning_middle_and_end_excludes_existing_overlays_and_roundtrips() {
        for after in [None, Some(frame_id(1)), Some(frame_id(4))] {
            let directory = tempfile::tempdir().unwrap();
            let mut workspace = create_rendered_duplicate_workspace(&directory);
            workspace.select_all();
            workspace
                .add_overlay_for_selection(
                    "Watermark on originals".to_owned(),
                    OverlayContent::Shape {
                        kind: ShapeKind::Rectangle,
                        bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                        stroke_width: 0,
                        stroke: GREEN,
                        fill: Some(GREEN),
                    },
                    100,
                    255,
                    BlendMode::Normal,
                )
                .unwrap();
            workspace
                .execute(EditCommand::SetTransitions {
                    transitions: [(1, 2), (3, 4)]
                        .into_iter()
                        .map(|(from, to)| Transition {
                            id: TransitionId::from_u128(from),
                            from_frame: frame_id(from),
                            to_frame: frame_id(to),
                            duration: DurationUs::new(1).unwrap(),
                            steps: 1,
                            kind: TransitionKind::FadeToNext,
                        })
                        .collect(),
                })
                .unwrap();
            let original = workspace.manifest().clone();
            let (request, image) = text("Title", BLUE);
            let title = TitleFrameRequest {
                after,
                duration: DurationUs::new(50).unwrap(),
                background: BLACK,
                text: request,
                position: PhysicalPoint::default(),
            };
            let id = workspace.insert_title_frame(&title, &image).unwrap();
            let index = match after {
                None => 0,
                Some(id) if id == frame_id(1) => 1,
                _ => 4,
            };
            assert_eq!(workspace.selection().current(), Some(id));
            assert_eq!(
                workspace.manifest().revision.get(),
                original.revision.get() + 1
            );
            let inserted = &workspace.manifest().timeline.frames[index];
            assert_eq!(
                inserted.transform.output_size,
                Some(workspace.manifest().canvas.size)
            );
            assert_eq!(workspace.manifest().assets[&inserted.asset_id].byte_len, 4);
            let start = workspace.manifest().timeline.frame_start(id).unwrap().get();
            let old_track = &workspace.manifest().timeline.overlay_tracks[0];
            assert_eq!(old_track.items.len(), if index == 1 { 2 } else { 1 });
            for item in &old_track.items {
                assert!(
                    item.span.end().unwrap().get() <= start || item.span.start.get() >= start + 50
                );
            }
            assert_eq!(
                old_track.items[0].id,
                original.timeline.overlay_tracks[0].items[0].id
            );
            assert_eq!(
                workspace.manifest().timeline.transitions.len(),
                if index == 1 { 1 } else { 2 }
            );
            let rendered =
                crate::editor_preview::render_frame_surface(workspace.active_project(), id, 1024)
                    .unwrap();
            assert_eq!(rendered.pixels(), &[0, 0, 255, 255, 0, 0, 0, 255]);
            let edited = workspace.manifest().clone();
            for _ in 0..10 {
                assert!(workspace.undo().unwrap());
                assert_project_content(workspace.manifest(), &original);
                assert!(workspace.redo().unwrap());
                assert_project_content(workspace.manifest(), &edited);
            }
            drop(workspace);
            let reopened =
                EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
            assert!(reopened.asset_issues().is_empty());
            assert_project_content(reopened.manifest(), &edited);
            let decoded = export_frames(&reopened, "title.gif");
            let title_frames = decoded
                .frames()
                .iter()
                .filter(|frame| frame.rgba() == [0, 0, 255, 255, 0, 0, 0, 255])
                .count();
            assert_eq!(title_frames, 1);
        }
    }

    #[test]
    fn title_insertion_splits_persistent_authoring_scope_without_covering_title_pixels() {
        use gif_from_screen_domain::{AnnotationMode, AnnotationRequest, ProgressOptions};
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&dir);
        workspace.select_all();
        let annotation = AnnotationRequest {
            size: workspace.manifest().canvas.size,
            mode: AnnotationMode::Progress(ProgressOptions {
                format: String::new(),
                ..ProgressOptions::default()
            }),
            ..AnnotationRequest::default()
        };
        workspace
            .apply_annotation_edit(
                &workspace.project_edit_anchor(),
                &annotation,
                &std::sync::atomic::AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let total = workspace
            .manifest()
            .timeline
            .total_duration()
            .unwrap()
            .get();
        let insertion = workspace.manifest().timeline.frames[0].duration.get();
        let mut original = workspace.manifest().timeline.overlay_tracks[0].clone();
        original.annotation_scope = Some(vec![TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(total).unwrap(),
        }]);
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: original.clone(),
            })
            .unwrap();
        let (text, image) = text("Title", BLUE);
        workspace
            .insert_title_frame(
                &TitleFrameRequest {
                    after: Some(frame_id(1)),
                    duration: DurationUs::new(50).unwrap(),
                    background: BLACK,
                    text,
                    position: PhysicalPoint::default(),
                },
                &image,
            )
            .unwrap();
        let scope = workspace.manifest().timeline.overlay_tracks[0]
            .annotation_scope
            .as_ref()
            .unwrap();
        assert_eq!(scope.len(), 2);
        assert_eq!(scope[0].start, TimeUs::ZERO);
        assert_eq!(scope[0].end().unwrap().get(), insertion);
        assert_eq!(scope[1].start.get(), insertion + 50);
        assert_eq!(scope[1].end().unwrap().get(), total + 50);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], original);
        workspace.redo().unwrap();
        let updated = workspace.manifest().timeline.overlay_tracks.clone();
        drop(workspace);
        let reopened = EditorWorkspace::open(dir.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().timeline.overlay_tracks, updated);
    }

    #[test]
    fn unknown_mixed_and_unprepared_text_groups_are_rejected_without_storing_assets() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        let (request, image) = text("First", GREEN);
        let id = workspace
            .add_text_overlay_for_selection(&request, &image, PhysicalPoint::default())
            .unwrap();
        let (replacement, replacement_image) = text("Changed", BLUE);
        for invalid in 0..3 {
            let mut track = workspace.manifest().timeline.overlay_tracks[0].clone();
            let target = if invalid == 0 { TrackId::NIL } else { id };
            if invalid == 1 {
                if let OverlayContent::Text { text, .. } = &mut track.items[1].content {
                    *text = "Different source".to_owned();
                }
                workspace
                    .execute(EditCommand::UpsertOverlayTrack { track })
                    .unwrap();
            } else if invalid == 2 {
                for item in &mut track.items {
                    if let OverlayContent::Text { raster, .. } = &mut item.content {
                        *raster = None;
                    }
                }
                workspace
                    .execute(EditCommand::UpsertOverlayTrack { track })
                    .unwrap();
            }
            let before = workspace.manifest().clone();
            assert!(matches!(
                workspace.replace_text_overlay(
                    target,
                    &replacement,
                    &replacement_image,
                    PhysicalPoint::default()
                ),
                Err(EditorWorkspaceError::TextTrackNotEditable(_))
            ));
            assert_project_content(workspace.manifest(), &before);
            assert!(
                !workspace
                    .active_project()
                    .assets()
                    .asset_path(gif_from_screen_project::AssetStore::id_for_bytes(
                        &replacement_image.rgba
                    ))
                    .exists()
            );
        }
    }

    #[test]
    fn title_preflight_rejects_stale_anchor_overflow_and_bad_pixels_without_mutation() {
        for invalid in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let mut workspace =
                create_workspace(&directory, &[if invalid == 1 { u64::MAX } else { 100 }], 8);
            let (request, mut image) = text("Title", BLUE);
            let title = TitleFrameRequest {
                after: if invalid == 0 {
                    Some(frame_id(99))
                } else {
                    None
                },
                duration: DurationUs::new(1).unwrap(),
                background: BLACK,
                text: request,
                position: PhysicalPoint::default(),
            };
            if invalid == 2 {
                image.rgba.pop();
            }
            let before = workspace.manifest().clone();
            assert!(workspace.insert_title_frame(&title, &image).is_err());
            assert_project_content(workspace.manifest(), &before);
            assert!(!workspace.can_undo());
            assert_eq!(
                std::fs::read_dir(workspace.active_project().assets().directory())
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[test]
    fn title_reuses_existing_overlay_pixels_as_background_without_changing_their_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace.select_only(frame_id(1)).unwrap();
        let (old_request, old_image) = text("Original", GREEN);
        workspace
            .add_text_overlay_for_selection(&old_request, &old_image, PhysicalPoint::default())
            .unwrap();
        let background_id = gif_from_screen_project::AssetStore::id_for_bytes(&old_image.rgba);
        let descriptor = workspace.manifest().assets[&background_id].clone();
        assert!(matches!(descriptor.kind, AssetKind::OverlayImage { .. }));
        let original_count = workspace.manifest().assets.len();
        let (request, image) = text("Title", BLUE);
        let mut title = TitleFrameRequest {
            after: Some(frame_id(1)),
            duration: DurationUs::new(50).unwrap(),
            background: GREEN,
            text: request,
            position: PhysicalPoint::default(),
        };
        let id = workspace.insert_title_frame(&title, &image).unwrap();
        assert_eq!(workspace.manifest().assets[&background_id], descriptor);
        assert_eq!(workspace.manifest().assets.len(), original_count + 1);
        assert_eq!(
            workspace.manifest().timeline.frames[1].asset_id,
            background_id
        );
        title.after = Some(id);
        let second_id = workspace.insert_title_frame(&title, &image).unwrap();
        assert_eq!(workspace.manifest().assets.len(), original_count + 1);
        assert_eq!(workspace.manifest().assets[&background_id], descriptor);
        let rendered = crate::editor_preview::render_frame_surface(
            workspace.active_project(),
            second_id,
            1024,
        )
        .unwrap();
        assert_eq!(rendered.pixels(), &[0, 0, 255, 255, 0, 255, 0, 255]);
        let decoded = export_frames(&workspace, "shared-background.gif");
        assert_eq!(
            decoded.frames()[1].rgba(),
            &[0, 0, 255, 255, 0, 255, 0, 255]
        );
    }

    #[test]
    fn title_can_start_an_empty_timeline_and_keep_its_background_transparent() {
        use gif_from_screen_domain::{Canvas, CanvasBackground, ColorSpace, ProjectId, UnixTimeMs};
        use gif_from_screen_project::ActiveProject;

        let directory = tempfile::tempdir().unwrap();
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(12),
            "test",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(2, 1).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut workspace = EditorWorkspace::from_active(
            ActiveProject::create(directory.path(), manifest).unwrap(),
            8,
        )
        .unwrap();
        let (request, image) = text("Title", BLUE);
        let title = TitleFrameRequest {
            after: None,
            duration: DurationUs::new(50).unwrap(),
            background: Rgba::TRANSPARENT,
            text: request,
            position: PhysicalPoint::default(),
        };
        let id = workspace.insert_title_frame(&title, &image).unwrap();
        assert_eq!(workspace.selection().current(), Some(id));
        let rendered =
            crate::editor_preview::render_frame_surface(workspace.active_project(), id, 1024)
                .unwrap();
        assert_eq!(rendered.pixels(), &[0, 0, 255, 255, 0, 0, 0, 0]);
        assert!(workspace.undo().unwrap());
        assert!(workspace.manifest().timeline.frames.is_empty());
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        assert!(workspace.manifest().assets.is_empty());
    }
}
