//! An owned X11 recorder's input hole, independent of its visual transparency.

use std::sync::atomic::{AtomicBool, Ordering};

use gif_from_screen_capture::{PhysicalRect, PhysicalSize};

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native;

/// Makes the center of this process's exact recorder window mouse-transparent.
///
/// Only the INPUT shape changes: its border and toolbar retain the outer-minus-hole
/// region, while visual/bounding shapes remain untouched. The input shape persists
/// after this function closes its private X11 connection. No window is destroyed.
/// The PID property and unpredictable exact title are accidental-targeting guards,
/// not an isolation boundary against malicious clients sharing an X11 server.
///
/// Returns `false` when the window is absent, disappears, changes identity, or has
/// not reached `expected_size` yet. Callers must bound retries and gate recording
/// on acknowledgement of the current geometry.
///
/// # Errors
/// Rejects a foreign PID, invalid title/rectangle, duplicate matching windows,
/// exhausted discovery limits, unavailable extensions, cancellation, or a bounded
/// native protocol failure. Setup and requests share the existing five-second X11
/// transport deadline; cleanup gets a separate bounded grace period.
pub fn set_recorder_input_shape(
    display: Option<&str>,
    owner_pid: u32,
    exact_title: &str,
    expected_size: PhysicalSize,
    hole: PhysicalRect,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    validate(owner_pid, exact_title, expected_size, hole)?;
    check_cancel(cancel)?;
    #[cfg(all(target_os = "linux", feature = "native-x11"))]
    {
        native::set(display, owner_pid, exact_title, expected_size, hole, cancel)
    }
    #[cfg(not(all(target_os = "linux", feature = "native-x11")))]
    {
        let _ = display;
        Err("This build does not include native X11 recorder input shapes.".into())
    }
}

fn validate(pid: u32, title: &str, size: PhysicalSize, hole: PhysicalRect) -> Result<(), String> {
    if pid != std::process::id() {
        return Err("Recorder input shapes may only target the current process.".into());
    }
    if title.is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
        return Err(
            "Recorder window identity must be a nonempty exact title of at most 256 bytes.".into(),
        );
    }
    if size.width() > u32::from(u16::MAX)
        || size.height() > u32::from(u16::MAX)
        || !hole.fits_within(size)
        || i16::try_from(hole.origin().x).is_err()
        || i16::try_from(hole.origin().y).is_err()
    {
        return Err("Recorder input hole must fit its positive client size and X11 rectangle coordinate limits.".into());
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Recorder input-shape preparation cancelled.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
