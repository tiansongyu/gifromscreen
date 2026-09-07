use super::*;

fn size() -> PhysicalSize {
    PhysicalSize::new(300, 220).unwrap()
}
fn hole() -> PhysicalRect {
    PhysicalRect::new(20, 20, 260, 150).unwrap()
}

#[test]
fn recorder_input_rejects_foreign_pid_bad_identity_and_geometry_before_connection() {
    let cancel = AtomicBool::new(false);
    let invalid_display = Some("never-resolve-this-host:0");
    assert!(
        set_recorder_input_shape(
            invalid_display,
            std::process::id() + 1,
            "owned",
            size(),
            hole(),
            &cancel
        )
        .unwrap_err()
        .contains("current process")
    );
    for title in [String::new(), "x".repeat(257), "owned\nwindow".to_owned()] {
        assert!(
            set_recorder_input_shape(
                invalid_display,
                std::process::id(),
                &title,
                size(),
                hole(),
                &cancel
            )
            .unwrap_err()
            .contains("exact title")
        );
    }
    for region in [
        PhysicalRect::new(-1, 0, 1, 1).unwrap(),
        PhysicalRect::new(0, 0, 301, 1).unwrap(),
    ] {
        assert!(
            set_recorder_input_shape(
                invalid_display,
                std::process::id(),
                "owned",
                size(),
                region,
                &cancel
            )
            .unwrap_err()
            .contains("must fit")
        );
    }
    let large = PhysicalSize::new(50_000, 100).unwrap();
    assert!(
        validate(
            std::process::id(),
            "owned",
            large,
            PhysicalRect::new(40_000, 0, 10, 10).unwrap()
        )
        .is_err()
    );
}

#[test]
fn recorder_input_accepts_zero_origin_and_cancellation_does_not_connect() {
    let edge = PhysicalRect::new(0, 0, 300, 170).unwrap();
    validate(std::process::id(), "owned recorder", size(), edge).unwrap();
    assert!(
        set_recorder_input_shape(
            Some("never-resolve-this-host:0"),
            std::process::id(),
            "owned recorder",
            size(),
            edge,
            &AtomicBool::new(true)
        )
        .unwrap_err()
        .contains("cancelled")
    );
}
