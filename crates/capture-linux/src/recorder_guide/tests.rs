use super::*;
use gif_from_screen_capture::PhysicalSize;

fn request(generation: u64) -> GuideRequest {
    GuideRequest {
        generation,
        region: Some(PhysicalRect::new(80, 60, 120, 90).unwrap()),
        protected_region: None,
        border_width: 4,
        handle_scale: 100,
        handle_avoid: None,
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
fn controller_observation_is_retained_but_cleared_on_stop_failure_and_new_session() {
    let (mut guide, send) = service();
    let geometry = ControllerGeometry {
        client: PhysicalRect::new(-745, 983, 120, 80).unwrap(),
        outer: PhysicalRect::new(-747, 950, 124, 115).unwrap(),
        viewable: true,
    };
    assert!(guide.poll().controller.is_none());
    guide.context.controller(geometry);
    assert_eq!(guide.poll().controller, Some(geometry));
    assert_eq!(guide.poll().controller, Some(geometry));
    let debug = format!("{geometry:?}");
    assert!(!debug.contains("745") && !debug.contains("983") && !debug.contains("950"));
    send.send(Err("lost owned controller".into())).unwrap();
    assert!(guide.poll().controller.is_none());
    let (mut next, _send) = service();
    assert!(next.poll().controller.is_none());
    next.context.controller(geometry);
    next.stop();
    next.context.controller(geometry);
    assert!(next.poll().controller.is_none());
}

#[test]
fn controller_constructor_rejects_foreign_pid_and_zero_window_without_connecting() {
    assert!(RecorderGuide::start_with_controller(Some("invalid-display".into()), 42, 0).is_err());
    assert!(
        RecorderGuide::start_with_controller(Some("invalid-display".into()), 0, std::process::id())
            .is_err()
    );
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
    let id = service
        .context
        .begin_gesture(1, service.context.cancel_epoch())
        .unwrap()
        .unwrap();
    for x in 0..1_000 {
        service.context.event(GuidePointerEvent::Moved {
            gesture_id: id,
            position: PhysicalPosition { x, y: 5 },
        });
    }
    assert_eq!(service.poll().events.len(), 1);
    for _ in 0..=EVENT_LIMIT {
        service.context.event(GuidePointerEvent::Pressed {
            generation: 1,
            gesture_id: id,
            position: PhysicalPosition { x: 4, y: 5 },
            edge: GuideEdge::Top,
            modifiers: 0,
        });
    }
    let update = service.poll();
    assert!(update.dropped_events > 0);
    assert_eq!(
        update.events,
        [GuidePointerEvent::Cancelled { gesture_id: id }]
    );
    service.stop();
    service.context.event(GuidePointerEvent::Released {
        gesture_id: id,
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
        gesture_id: 7,
        position: PhysicalPosition { x: 1234, y: 5678 },
    };
    assert!(!format!("{event:?}").contains("5678"));
}

#[test]
fn drag_handle_is_large_scaled_and_outside_capture() {
    let root = PhysicalSize::new(1600, 1200).unwrap();
    for scale in [100, 177, 200, 400] {
        let request = GuideRequest {
            region: Some(PhysicalRect::new(500, 500, 500, 400).unwrap()),
            handle_scale: scale,
            ..request(1)
        };
        let handle = geometry::drag_handle(request, root).unwrap().rect;
        assert_eq!(
            handle.size().width(),
            (144 * u32::from(scale)).div_ceil(100)
        );
        assert_eq!(
            handle.size().height(),
            (36 * u32::from(scale)).div_ceil(100)
        );
        assert!(geometry::intersection(handle, request.region.unwrap()).is_none());
        assert_eq!(
            i64::from(handle.origin().y) + i64::from(handle.size().height()),
            496
        );
    }
}

#[test]
fn drag_handle_uses_another_edge_and_avoids_the_controller_and_old_capture() {
    let root = PhysicalSize::new(400, 300).unwrap();
    let region = PhysicalRect::new(0, 0, 250, 120).unwrap();
    let right_controller = PhysicalRect::new(254, 0, 146, 300).unwrap();
    let request = GuideRequest {
        region: Some(region),
        handle_avoid: Some(right_controller),
        ..request(1)
    };
    let handle = geometry::drag_handle(request, root).unwrap().rect;
    assert_eq!(handle.origin().y, 124);
    assert!(geometry::intersection(handle, right_controller).is_none());
    assert!(geometry::intersection(handle, region).is_none());
    let request = GuideRequest {
        protected_region: Some(PhysicalRect::new(0, 124, 400, 176).unwrap()),
        ..request
    };
    assert!(geometry::drag_handle(request, root).is_none());
}

#[test]
fn short_top_edge_recording_keeps_a_side_grip_above_its_controller() {
    let root = PhysicalSize::new(400, 300).unwrap();
    let request = GuideRequest {
        region: Some(PhysicalRect::new(0, 0, 120, 80).unwrap()),
        handle_avoid: Some(PhysicalRect::new(0, 88, 400, 212).unwrap()),
        ..request(1)
    };
    assert_eq!(
        geometry::drag_handle(request, root).unwrap().rect,
        PhysicalRect::new(124, 0, 36, 80).unwrap()
    );
}

#[test]
fn drag_handle_never_invades_capture_on_small_offscreen_or_full_root_regions() {
    let root_size = PhysicalSize::new(400, 300).unwrap();
    let root = PhysicalRect::new(0, 0, 400, 300).unwrap();
    for x in [-20, 0, 1, 100, 390, 400] {
        for y in [-20, 0, 1, 100, 290, 300] {
            for (width, height) in [(1, 1), (10, 10), (120, 90), (400, 300)] {
                let region = PhysicalRect::new(x, y, width, height).unwrap();
                let request = GuideRequest {
                    region: Some(region),
                    ..request(1)
                };
                if let Some(handle) = geometry::drag_handle(request, root_size) {
                    assert_eq!(geometry::intersection(handle.rect, root), Some(handle.rect));
                    assert!(geometry::intersection(handle.rect, region).is_none());
                }
            }
        }
    }
    for region in [
        None,
        Some(root),
        Some(PhysicalRect::new(450, 30, 40, 40).unwrap()),
    ] {
        assert!(
            geometry::drag_handle(
                GuideRequest {
                    region,
                    ..request(1)
                },
                root_size
            )
            .is_none()
        );
    }
    for scale in [0, 99, 401, u16::MAX] {
        assert!(
            geometry::validate(GuideRequest {
                handle_scale: scale,
                ..request(1)
            })
            .is_err()
        );
    }
}

#[test]
fn active_gesture_survives_new_presentations_and_stale_cancel_cannot_end_the_next_press() {
    let (mut service, _send) = service();
    service.request(request(1)).unwrap();
    service.context.acknowledge(GuideAck {
        generation: 1,
        visible: true,
    });
    let first = service.context.begin_gesture(1, 0).unwrap().unwrap();
    service.context.event(GuidePointerEvent::Pressed {
        generation: 1,
        gesture_id: first,
        position: PhysicalPosition { x: 1, y: 2 },
        edge: GuideEdge::Top,
        modifiers: 0,
    });
    service
        .request(GuideRequest {
            region: None,
            ..request(2)
        })
        .unwrap();
    service.context.event(GuidePointerEvent::Moved {
        gesture_id: first,
        position: PhysicalPosition { x: 3, y: 4 },
    });
    assert_eq!(service.poll().events.len(), 2);
    assert!(service.context.active_gesture(first));
    service.cancel_gesture();
    assert_eq!(
        service.poll().events,
        [GuidePointerEvent::Cancelled { gesture_id: first }]
    );
    service.request(request(3)).unwrap();
    service.context.acknowledge(GuideAck {
        generation: 3,
        visible: true,
    });
    assert!(
        service.context.begin_gesture(3, 0).unwrap().is_none(),
        "unserviced cancellation epoch must block old queued presses"
    );
    let epoch = service.context.cancel_epoch();
    let second = service.context.begin_gesture(3, epoch).unwrap().unwrap();
    assert!(second > first);
    service
        .context
        .event(GuidePointerEvent::Cancelled { gesture_id: first });
    service.context.event(GuidePointerEvent::Released {
        gesture_id: first,
        position: PhysicalPosition { x: 3, y: 4 },
    });
    assert!(service.poll().events.is_empty());
    assert!(service.context.active_gesture(second));
    service.context.event(GuidePointerEvent::Released {
        gesture_id: second,
        position: PhysicalPosition { x: 3, y: 4 },
    });
    assert_eq!(service.poll().events.len(), 1);
    let third = service.context.begin_gesture(3, epoch).unwrap().unwrap();
    assert!(
        third > second,
        "two presses in the same presentation need unique gesture ids"
    );
}

#[test]
fn a_new_guide_never_reuses_an_old_recording_gesture_identity() {
    let (old, _old_sender) = service();
    old.request(request(1)).unwrap();
    old.context.acknowledge(GuideAck {
        generation: 1,
        visible: true,
    });
    let old_id = old.context.begin_gesture(1, 0).unwrap().unwrap();
    old.stop();
    let (mut new, _new_sender) = service();
    new.request(request(1)).unwrap();
    new.context.acknowledge(GuideAck {
        generation: 1,
        visible: true,
    });
    let new_id = new.context.begin_gesture(1, 0).unwrap().unwrap();
    assert!(new_id > old_id);
    new.context
        .event(GuidePointerEvent::Cancelled { gesture_id: old_id });
    assert!(new.poll().events.is_empty());
    assert!(new.context.active_gesture(new_id));
}
