use super::*;
use gif_from_screen_domain::{CaptureClockContext, CaptureClockId, DurationUs, KeyStroke};

fn populated(frames: usize) -> ProjectManifest {
    let mut manifest = super::super::tests::manifest();
    let base = manifest.timeline.frames[0].clone();
    manifest.timeline.frames.clear();
    for index in 0..frames {
        let mut frame = base.clone();
        frame.id = FrameId::from_u128(index as u128 + 1);
        frame.duration = DurationUs::new(1000).unwrap();
        frame.capture_clock = Some(CaptureClockContext {
            id: Some(CaptureClockId::from_u128(1)),
            sampled_at: TimeUs::new(index as u64 * 1000),
        });
        frame.capture_metadata.captured_at = Some(TimeUs::new(index as u64 * 1000));
        if index == 0 {
            frame.capture_metadata.key_strokes.push(KeyStroke {
                physical_key: "A".to_owned(),
                display_text: Some("A".to_owned()),
                pressed: true,
                at: TimeUs::ZERO,
                repeat: false,
                modifiers: 0,
            });
        }
        manifest.timeline.frames.push(frame);
    }
    manifest
}

#[test]
fn ten_thousand_empty_delivery_frames_share_one_small_input_pool() {
    let manifest = populated(10_000);
    let selected = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let prepared = super::super::prepare_annotations_with_assets(
        &manifest,
        &selected,
        &AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            hold_ms: 60_000,
            ..AnnotationRequest::default()
        },
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    let pools = prepared
        .assets
        .iter()
        .filter(|(asset, _)| matches!(&asset.kind, AssetKind::ImportedSource { .. }))
        .collect::<Vec<_>>();
    assert_eq!(pools.len(), 1);
    assert!(pools[0].1.len() < 1024);
    let pool: FrameInputReplayPool = serde_json::from_slice(&pools[0].1).unwrap();
    assert_eq!(pool.steps.len(), 1);
    let EditCommand::UpsertOverlayTrack { track } = prepared.commands.last().unwrap() else {
        panic!("track")
    };
    let cells = track.frame_cells.as_ref().unwrap();
    assert_eq!(cells.len(), 10_000);
    assert!(
        cells
            .iter()
            .all(|cell| cell.input_replay.as_ref().unwrap().runs[0].asset_id == pools[0].0.id)
    );
    assert_eq!(
        cells.last().unwrap().input_replay.as_ref().unwrap().runs[0].step_end,
        1
    );
}

#[test]
fn replay_pool_budget_is_checked_before_cloning_a_large_run() {
    let mut manifest = populated(8);
    for frame in &mut manifest.timeline.frames[1..] {
        frame.capture_metadata.key_strokes = vec![
            KeyStroke {
                physical_key: "B".repeat(4096),
                display_text: None,
                pressed: false,
                at: frame.capture_sample_time().unwrap(),
                repeat: false,
                modifiers: 0,
            };
            512
        ];
    }
    let selected = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let result = super::super::prepare_annotations_with_assets(
        &manifest,
        &selected,
        &AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        },
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    );
    assert!(result.unwrap_err().contains("8 MiB"));
}

#[test]
fn replay_output_limits_fail_before_retaining_excess_marks_or_combined_text() {
    let owner = FrameId::from_u128(1);
    let mut output = ReplayOutput {
        marks: super::super::MAX_ITEMS,
        ..ReplayOutput::default()
    };
    assert!(
        output
            .push(
                owner,
                OverlayContent::KeyStroke {
                    text: "A".to_owned(),
                    position: gif_from_screen_domain::PhysicalPoint::default(),
                    raster: None,
                }
            )
            .is_err()
    );
    assert!(output.contents.is_empty());
    output.text(owner, 1, "A".repeat(4096)).unwrap();
    assert!(output.text(owner, 2, "B".to_owned()).is_err());
    assert_eq!(output.texts[&owner].len(), 1);
}

fn staged_input_project() -> (ProjectManifest, RgbaSurface) {
    use gif_from_screen_domain::{
        ClipTransform, FrameRenderStep, MouseButton, MouseInputEvent, PhysicalPoint, PhysicalPx,
        PhysicalRect, PhysicalSize, RasterEncoding,
    };
    let mut manifest = populated(1);
    let cursor =
        RgbaSurface::new(PhysicalSize::new(1, 1).unwrap(), vec![255, 255, 255, 128]).unwrap();
    let cursor_id = AssetStore::id_for_bytes(cursor.pixels());
    manifest.assets.insert(
        cursor_id,
        AssetDescriptor {
            id: cursor_id,
            byte_len: 4,
            kind: AssetKind::OverlayImage {
                size: cursor.size(),
                encoding: RasterEncoding::Rgba8,
            },
        },
    );
    let frame = &mut manifest.timeline.frames[0];
    frame.transform = ClipTransform {
        crop: Some(PhysicalRect::new(10, 0, 20, 20).unwrap()),
        output_size: Some(PhysicalSize::new(40, 20).unwrap()),
        ..ClipTransform::default()
    };
    frame.render_steps = vec![
        FrameRenderStep::Composite { stage_id: 11 },
        FrameRenderStep::Resize {
            size: PhysicalSize::new(20, 10).unwrap(),
        },
    ];
    let point = PhysicalPoint {
        x: PhysicalPx::new(20),
        y: PhysicalPx::new(10),
    };
    frame.capture_metadata.cursor_visible = true;
    frame.capture_metadata.cursor_asset = Some(cursor_id);
    frame.capture_metadata.cursor_position = Some(point);
    frame.capture_metadata.mouse_events.push(MouseInputEvent {
        at: TimeUs::ZERO,
        button: MouseButton::Left,
        pressed: true,
        position: Some(point),
    });
    manifest.canvas.size = PhysicalSize::new(20, 10).unwrap();
    (manifest, cursor)
}

fn position(prepared: &PreparedAnnotations) -> (u32, u32) {
    let track = super::super::tests::track(prepared);
    let content = track.all_mark_contents().next().unwrap().1;
    let (OverlayContent::MouseClick {
        position: point, ..
    }
    | OverlayContent::Cursor {
        position: point, ..
    }) = content
    else {
        panic!("expected positional input mark")
    };
    (point.x.get(), point.y.get())
}

fn seal_and_extend_input_program(manifest: &mut ProjectManifest) {
    use gif_from_screen_domain::{
        Effect, FrameRenderStep, PhysicalRect, PhysicalSize, QuarterTurn,
    };
    manifest.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0]
        .stage = Some(22);
    manifest.timeline.frames[0].render_steps.extend([
        FrameRenderStep::Composite { stage_id: 22 },
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        FrameRenderStep::Crop {
            rect: PhysicalRect::new(2, 0, 8, 20).unwrap(),
        },
        FrameRenderStep::Effect {
            effect: Effect::Blur {
                region: PhysicalRect::new(0, 0, 8, 20).unwrap(),
                radius: 1,
            },
        },
    ]);
    manifest.canvas.size = PhysicalSize::new(8, 20).unwrap();
}

#[test]
fn recorded_click_and_cursor_creation_reedit_and_legacy_replay_use_their_distinct_stages() {
    for mode in [
        AnnotationMode::RecordedClicks,
        AnnotationMode::RecordedCursor,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store = AssetStore::open(directory.path()).unwrap();
        let (mut manifest, cursor) = staged_input_project();
        let raw = manifest.timeline.frames[0].capture_metadata.clone();
        let clock = manifest.timeline.frames[0].capture_clock;
        let selected = [manifest.timeline.frames[0].id].into_iter().collect();
        let request = AnnotationRequest {
            mode,
            ..AnnotationRequest::default()
        };
        let provider = |_| Ok(cursor.clone());
        let prepared = super::super::prepare_annotations_with_assets(
            &manifest,
            &selected,
            &request,
            &AtomicBool::new(false),
            |_| {},
            &provider,
        )
        .unwrap();
        assert_eq!(position(&prepared), (10, 5));
        for (asset, bytes) in &prepared.assets {
            assert_eq!(store.put(bytes).unwrap(), asset.id);
        }
        manifest
            .apply_command(&EditCommand::Compound {
                commands: prepared.commands,
            })
            .unwrap();
        seal_and_extend_input_program(&mut manifest);
        let original = manifest.timeline.overlay_tracks[0].clone();
        let updated = prepare_replacement(
            &manifest,
            &original,
            &request,
            &store,
            &AtomicBool::new(false),
            |_| {},
            &provider,
        )
        .unwrap();
        assert_eq!(position(&updated), (10, 5));
        assert_eq!(
            super::super::tests::track(&updated)
                .frame_cells
                .as_ref()
                .unwrap()[0]
                .stage,
            Some(22)
        );
        let tail = super::super::prepare_annotations_with_assets(
            &manifest,
            &selected,
            &request,
            &AtomicBool::new(false),
            |_| {},
            &provider,
        )
        .unwrap();
        assert_eq!(position(&tail), (2, 10));
        assert_eq!(
            super::super::tests::track(&tail)
                .frame_cells
                .as_ref()
                .unwrap()[0]
                .stage,
            None
        );
        let legacy = super::super::prepare_legacy_annotations_with_assets(
            &manifest,
            &selected,
            &request,
            &AtomicBool::new(false),
            |_| {},
            &provider,
        )
        .unwrap();
        assert_eq!(position(&legacy), (20, 10));
        assert_eq!(manifest.timeline.frames[0].capture_metadata, raw);
        assert_eq!(manifest.timeline.frames[0].capture_clock, clock);
    }
}
