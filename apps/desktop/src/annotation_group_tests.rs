//! Same-named copied groups remain distinguishable without renaming project data.

use eframe::egui;
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, BlendMode, DurationUs, EditCommand, FrameAuthoringSpan,
    FrameId, FrameLocalSpan, FrameOverlayCell, OverlayContent, OverlayId, OverlayItem,
    OverlayTrack, PhysicalPoint, TimeUs, TimelineSpan, TrackId,
};
use gif_from_screen_project::LockPolicy;

use super::{AnnotationTools, EditorWorkspace, overlay_group_label, tests::legacy_workspace};

fn group(id: u128) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(id),
        name: "Recorded keys".to_owned(),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        annotation: Some(AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            hold_ms: u32::try_from(id).unwrap(),
            ..AnnotationRequest::default()
        }),
        annotation_scope: None,
        frame_cells: None,
        items: Vec::new(),
    }
}

fn cells(count: u128) -> Vec<FrameOverlayCell> {
    (1..=count)
        .map(|id| FrameOverlayCell {
            frame_id: FrameId::from_u128(id),
            stage: None,
            scopes: vec![FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::WHOLE,
            }],
            marks: Vec::new(),
            input_replay: None,
        })
        .collect()
}

fn timed_item(id: u128) -> OverlayItem {
    OverlayItem {
        id: OverlayId::from_u128(id),
        z_index: 0,
        span: TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(100_000).unwrap(),
        },
        content: OverlayContent::Cursor {
            cursor_asset: None,
            position: PhysicalPoint::default(),
            hotspot: PhysicalPoint::default(),
        },
    }
}

fn duplicate_groups(root: &std::path::Path) -> EditorWorkspace {
    let mut workspace = legacy_workspace(root);
    let mut base = group(10);
    base.name = "Background".to_owned();
    base.annotation = None;
    let mut empty = group(20);
    empty.frame_cells = Some(cells(0));
    let mut timed = group(30);
    timed.items.push(timed_item(31));
    let mut copied = group(40);
    copied.frame_cells = Some(cells(1));
    for track in [base, empty, timed, copied] {
        workspace
            .execute(EditCommand::UpsertOverlayTrack { track })
            .unwrap();
    }
    workspace
}

fn draw(
    context: &egui::Context,
    tools: &mut AnnotationTools,
    tracks: &[OverlayTrack],
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 480.0),
            )),
            events,
            ..egui::RawInput::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| tools.show_group_selector(ui, tracks));
        },
    )
}

fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => Some(
                egui::Rect::from_min_size(text.pos, text.galley.size()).intersect(shape.clip_rect),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing selector label: {label}"))
}

fn click(
    context: &egui::Context,
    tools: &mut AnnotationTools,
    tracks: &[OverlayTrack],
    position: egui::Pos2,
) -> egui::FullOutput {
    let mut output = None;
    for pressed in [true, false] {
        output = Some(draw(
            context,
            tools,
            tracks,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ));
    }
    output.unwrap()
}

#[test]
fn labels_distinguish_empty_owned_frames_and_timed_items_with_correct_plural_counts() {
    for (count, ending) in [(0, "0 frames"), (1, "1 frame"), (2, "2 frames")] {
        let mut track = group(20);
        track.frame_cells = Some(cells(count));
        assert_eq!(
            overlay_group_label(1, &track),
            format!("Layer 2 · Recorded keys · {ending}")
        );
        assert!(
            track
                .frame_cells
                .as_ref()
                .unwrap()
                .iter()
                .all(|cell| cell.marks.is_empty())
        );
    }
    for (count, ending) in [
        (0, "0 timed items"),
        (1, "1 timed item"),
        (2, "2 timed items"),
    ] {
        let mut track = group(30);
        track.items = (0..count).map(timed_item).collect();
        assert_eq!(
            overlay_group_label(2, &track),
            format!("Layer 3 · Recorded keys · {ending}")
        );
    }
}

#[test]
fn duplicate_names_select_by_track_id_and_keep_identical_selected_and_option_labels() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = duplicate_groups(&directory.path().join("groups.gfsproj"));
    let before = workspace.manifest().clone();
    let journal = std::fs::read(workspace.project_root().join("journal.ndjson")).unwrap();
    let tracks = &workspace.manifest().timeline.overlay_tracks;
    let mut tools = AnnotationTools::default();
    let context = egui::Context::default();
    let mut selected_label = "New annotation group".to_owned();
    for (index, id) in [(1, 20), (3, 40), (2, 30)] {
        let initial = draw(&context, &mut tools, tracks, Vec::new());
        let selector = text_rect(&initial, &selected_label);
        click(&context, &mut tools, tracks, selector.center());
        let opened = draw(&context, &mut tools, tracks, Vec::new());
        for (index, track) in tracks.iter().enumerate().skip(1) {
            assert!(text_rect(&opened, &overlay_group_label(index, track)).is_positive());
        }
        let label = overlay_group_label(index, &tracks[index]);
        let target = text_rect(&opened, &label);
        click(&context, &mut tools, tracks, target.center());
        assert_eq!(tools.replacing, Some(TrackId::from_u128(id)));
        assert_eq!(tools.request.hold_ms, u32::try_from(id).unwrap());
        let closed = draw(&context, &mut tools, tracks, Vec::new());
        assert!(text_rect(&closed, &label).is_positive());
        selected_label = label;
    }
    assert_eq!(workspace.manifest(), &before);
    assert_eq!(
        std::fs::read(workspace.project_root().join("journal.ndjson")).unwrap(),
        journal
    );
    assert!(!tools.is_running());
}

#[test]
fn reopening_preserves_layer_positions_labels_and_source_names() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("reopen.gfsproj");
    let workspace = duplicate_groups(&root);
    let before = workspace.manifest().clone();
    let labels: Vec<_> = before
        .timeline
        .overlay_tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.id, overlay_group_label(index, track)))
        .collect();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(reopened.manifest(), &before);
    let tracks = &reopened.manifest().timeline.overlay_tracks;
    assert_eq!(
        tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (track.id, overlay_group_label(index, track)))
            .collect::<Vec<_>>(),
        labels
    );
    let mut tools = AnnotationTools {
        replacing: Some(TrackId::from_u128(40)),
        ..AnnotationTools::default()
    };
    let output = draw(&egui::Context::default(), &mut tools, tracks, Vec::new());
    assert!(text_rect(&output, "Layer 4 · Recorded keys · 1 frame").is_positive());
    assert_eq!(tools.replacing, Some(TrackId::from_u128(40)));
}
