use super::*;
use gif_from_screen_capture::PhysicalSize;

fn request(generation: u64) -> GuideRequest {
    GuideRequest {
        generation,
        region: Some(PhysicalRect::new(80, 60, 120, 90).unwrap()),
        protected_region: None,
        border_width: 4,
    }
}

fn service() -> (RecorderGuide, mpsc::SyncSender<Result<(), String>>) {
    let (send, receive) = mpsc::sync_channel(1);
    (
        RecorderGuide {
            context: new_context(),
            result: Some(receive),
        },
        send,
    )
}

#[test]
fn strict_generations_coalesce_and_never_acknowledge_a_superseded_request() {
    let (mut service, _send) = service();
    service.context.connected();
    assert_eq!(service.poll().status, GuideStatus::Ready);
    service.request(request(1)).unwrap();
    service.request(request(3)).unwrap();
    assert!(service.request(request(2)).is_err());
    assert!(service.request(request(3)).is_err());
    assert_eq!(service.context.request(), Some(request(3)));
    service.context.acknowledge(GuideAck {
        generation: 1,
        visible: true,
    });
    assert!(service.poll().ack.is_none());
    service.context.acknowledge(GuideAck {
        generation: 3,
        visible: true,
    });
    assert_eq!(
        service.poll().ack,
        Some(GuideAck {
            generation: 3,
            visible: true
        })
    );
    assert_eq!(
        service.poll().ack,
        Some(GuideAck {
            generation: 3,
            visible: true
        })
    );
}

#[test]
fn motion_coalesces_overload_cancels_and_stop_suppresses_late_input() {
    let (mut service, send) = service();
    service.request(request(1)).unwrap();
    service.context.acknowledge(GuideAck {
        generation: 1,
        visible: true,
    });
    for x in 0..1_000 {
        service.context.event(GuidePointerEvent::Moved {
            generation: 1,
            position: PhysicalPosition { x, y: 5 },
        });
    }
    assert_eq!(service.poll().events.len(), 1);
    for _ in 0..=EVENT_LIMIT {
        service.context.event(GuidePointerEvent::Pressed {
            generation: 1,
            position: PhysicalPosition { x: 4, y: 5 },
            edge: GuideEdge::Top,
            modifiers: 0,
        });
    }
    let update = service.poll();
    assert!(update.dropped_events > 0);
    assert_eq!(
        update.events,
        [GuidePointerEvent::Cancelled { generation: 1 }]
    );
    service.stop();
    service.context.event(GuidePointerEvent::Released {
        generation: 1,
        position: PhysicalPosition { x: 5, y: 6 },
    });
    assert!(service.poll().events.is_empty());
    assert!(service.is_running());
    assert!(service.request(request(2)).is_err());
    send.send(Ok(())).unwrap();
    assert_eq!(service.poll().status, GuideStatus::Stopped);
    assert!(!service.is_running());
}

#[test]
fn geometry_strips_never_overlap_capture_and_support_signed_positions() {
    for region in [
        PhysicalRect::new(80, 60, 120, 90).unwrap(),
        PhysicalRect::new(-40, -30, 120, 90).unwrap(),
    ] {
        let strips = geometry::strips(region, 4).unwrap();
        for strip in strips {
            assert!(geometry::intersection(strip.rect, region).is_none());
        }
    }
    assert!(geometry::covers_root(
        PhysicalRect::new(-1, -1, 402, 302).unwrap(),
        PhysicalSize::new(400, 300).unwrap()
    ));
    assert!(!geometry::covers_root(
        PhysicalRect::new(1, 0, 399, 300).unwrap(),
        PhysicalSize::new(400, 300).unwrap()
    ));
    for border in [0, 17, u16::MAX] {
        assert!(
            geometry::validate(GuideRequest {
                border_width: border,
                ..request(1)
            })
            .is_err()
        );
    }
    assert!(geometry::validate(request(0)).is_err());
    assert!(
        geometry::validate(GuideRequest {
            region: Some(PhysicalRect::new(i32::MIN, 0, 20, 20).unwrap()),
            ..request(1)
        })
        .is_err()
    );
}

#[test]
fn debug_output_does_not_disclose_capture_or_pointer_coordinates() {
    let request = GuideRequest {
        region: Some(PhysicalRect::new(1234, 5678, 901, 234).unwrap()),
        ..request(7)
    };
    assert!(!format!("{request:?}").contains("1234"));
    let event = GuidePointerEvent::Moved {
        generation: 7,
        position: PhysicalPosition { x: 1234, y: 5678 },
    };
    assert!(!format!("{event:?}").contains("5678"));
}
