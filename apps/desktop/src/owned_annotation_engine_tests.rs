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
