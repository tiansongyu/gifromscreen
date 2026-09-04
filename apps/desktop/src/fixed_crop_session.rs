//! Source-local fixed-canvas crop adapter for a prepared full-frame session.

use std::time::Duration;

use gif_from_screen_capture::{
    CaptureError, CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSessionState,
    CaptureSourceId, CaptureTarget, CapturedFrame, CursorMetadata, FramePoll, InputEvent,
    PhysicalPosition, PhysicalRect, PhysicalSize, PixelFormat, RecoveryHint,
};

/// Crops a full-source native session after its dimensions were learned from a
/// preparation frame. The output canvas is immutable; target updates may move
/// it within the same source but can never resize it.
pub(crate) struct FixedCropSession {
    inner: Box<dyn CaptureSession>,
    source_id: CaptureSourceId,
    source_size: PhysicalSize,
    canvas_size: PhysicalSize,
    crop: PhysicalRect,
    request: CaptureRequest,
}

impl std::fmt::Debug for FixedCropSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FixedCropSession")
            .field("source_id", &self.source_id)
            .field("source_size", &self.source_size)
            .field("crop", &self.crop)
            .field("state", &self.inner.state())
            .finish_non_exhaustive()
    }
}

impl FixedCropSession {
    /// Wraps one paused, full-source session and freezes its output dimensions.
    pub(crate) fn new(
        inner: Box<dyn CaptureSession>,
        source_id: CaptureSourceId,
        source_size: PhysicalSize,
        crop: PhysicalRect,
    ) -> Result<Self, CaptureError> {
        if inner.state() != CaptureSessionState::Paused {
            return Err(invalid_transition(
                "prepared crop session must be paused before its canvas is frozen",
            ));
        }
        if !crop.fits_within(source_size) {
            return Err(invalid_target(
                "prepared crop rectangle is outside the full Wayland source",
            ));
        }
        let mut request = inner.request().clone();
        request.target = CaptureTarget::Region {
            source: source_id.clone(),
            region: crop,
        };
        Ok(Self {
            inner,
            source_id,
            source_size,
            canvas_size: crop.size(),
            crop,
            request,
        })
    }

    fn validate_target(&self, target: &CaptureTarget) -> Result<PhysicalRect, CaptureError> {
        let CaptureTarget::Region { source, region } = target else {
            return Err(invalid_target(
                "a prepared Wayland crop can only move within its selected source",
            ));
        };
        if source != &self.source_id {
            return Err(CaptureError::new(
                CaptureErrorKind::SourceNotFound,
                "a prepared Wayland crop cannot switch portal-selected sources",
                RecoveryHint::ChooseDifferentSource,
            ));
        }
        if region.size() != self.canvas_size {
            return Err(invalid_target(
                "a prepared Wayland crop must preserve its frozen output dimensions",
            ));
        }
        if !region.fits_within(self.source_size) {
            return Err(invalid_target(
                "updated Wayland crop is outside the full source frame",
            ));
        }
        Ok(*region)
    }
}

impl CaptureSession for FixedCropSession {
    fn state(&self) -> CaptureSessionState {
        self.inner.state()
    }

    fn request(&self) -> &CaptureRequest {
        &self.request
    }

    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
        if !matches!(
            self.state(),
            CaptureSessionState::Recording | CaptureSessionState::Paused
        ) {
            return Err(invalid_transition(
                "cannot move a prepared Wayland crop after capture is terminal",
            ));
        }
        let crop = self.validate_target(&target)?;
        self.crop = crop;
        self.request.target = target;
        Ok(())
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        self.inner.pause()
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        self.inner.resume()
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        self.inner.stop()
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        self.inner.discard()
    }

    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
        match self.inner.poll_frame(timeout)? {
            FramePoll::Frame(frame) => {
                crop_frame(&frame, self.source_size, self.crop).map(FramePoll::Frame)
            }
            FramePoll::Pending => Ok(FramePoll::Pending),
            FramePoll::EndOfStream => Ok(FramePoll::EndOfStream),
        }
    }
}

fn crop_frame(
    frame: &CapturedFrame,
    source_size: PhysicalSize,
    crop: PhysicalRect,
) -> Result<CapturedFrame, CaptureError> {
    if frame.size() != source_size {
        return Err(CaptureError::invalid_frame(format!(
            "prepared Wayland source changed from {}x{} to {}x{}",
            source_size.width(),
            source_size.height(),
            frame.size().width(),
            frame.size().height()
        )));
    }
    let source_width = usize::try_from(source_size.width())
        .map_err(|_| CaptureError::invalid_frame("source width exceeds usize"))?;
    let source_height = usize::try_from(source_size.height())
        .map_err(|_| CaptureError::invalid_frame("source height exceeds usize"))?;
    let source_row_bytes = source_width
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("source row byte length overflowed"))?;
    if frame.stride() < source_row_bytes {
        return Err(CaptureError::invalid_frame(
            "prepared source frame stride is shorter than one row",
        ));
    }
    let required_source = frame
        .stride()
        .checked_mul(source_height)
        .ok_or_else(|| CaptureError::invalid_frame("source frame byte length overflowed"))?;
    if frame.pixels().len() < required_source {
        return Err(CaptureError::invalid_frame(
            "prepared source frame pixel buffer is truncated",
        ));
    }
    if !crop.fits_within(source_size) {
        return Err(invalid_target(
            "prepared crop is outside the current source frame",
        ));
    }

    let output_width = usize::try_from(crop.size().width())
        .map_err(|_| CaptureError::invalid_frame("crop width exceeds usize"))?;
    let output_height = usize::try_from(crop.size().height())
        .map_err(|_| CaptureError::invalid_frame("crop height exceeds usize"))?;
    let output_stride = output_width
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("crop row byte length overflowed"))?;
    let output_len = output_stride
        .checked_mul(output_height)
        .ok_or_else(|| CaptureError::invalid_frame("crop frame byte length overflowed"))?;
    let crop_x = usize::try_from(crop.origin().x)
        .map_err(|_| invalid_target("crop x coordinate is negative"))?;
    let crop_y = usize::try_from(crop.origin().y)
        .map_err(|_| invalid_target("crop y coordinate is negative"))?;
    let crop_byte_x = crop_x
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("crop x byte offset overflowed"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| CaptureError::invalid_frame("could not allocate cropped RGBA frame"))?;
    for y in 0..output_height {
        let source_start = crop_y
            .checked_add(y)
            .and_then(|row| row.checked_mul(frame.stride()))
            .and_then(|offset| offset.checked_add(crop_byte_x))
            .ok_or_else(|| CaptureError::invalid_frame("crop source offset overflowed"))?;
        let source_end = source_start
            .checked_add(output_stride)
            .ok_or_else(|| CaptureError::invalid_frame("crop source row end overflowed"))?;
        let row = frame
            .pixels()
            .get(source_start..source_end)
            .ok_or_else(|| CaptureError::invalid_frame("crop row exceeds source pixels"))?;
        match frame.format() {
            PixelFormat::Rgba8 => output.extend_from_slice(row),
            PixelFormat::Bgra8 => {
                for pixel in row.as_chunks::<4>().0 {
                    output.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
            _ => {
                return Err(CaptureError::invalid_frame(
                    "unsupported future prepared source pixel format",
                ));
            }
        }
    }
    let cropped = CapturedFrame::new(
        frame.sequence(),
        frame.captured_at(),
        crop.size(),
        output_stride,
        PixelFormat::Rgba8,
        output,
    )?;
    copy_cropped_metadata(frame, cropped, crop)
}

fn copy_cropped_metadata(
    source: &CapturedFrame,
    mut cropped: CapturedFrame,
    crop: PhysicalRect,
) -> Result<CapturedFrame, CaptureError> {
    cropped = cropped.with_damage(crop_damage(source.damage(), crop)?)?;
    if let Some(cursor) = source.cursor() {
        cropped = cropped.with_cursor(crop_cursor(cursor, crop));
    }
    Ok(cropped.with_input_events(
        source
            .input_events()
            .iter()
            .map(|event| crop_input_event(event, crop))
            .collect(),
    ))
}

fn crop_damage(
    damage: &[PhysicalRect],
    crop: PhysicalRect,
) -> Result<Vec<PhysicalRect>, CaptureError> {
    let crop_left = i64::from(crop.origin().x);
    let crop_top = i64::from(crop.origin().y);
    let crop_right = crop_left + i64::from(crop.size().width());
    let crop_bottom = crop_top + i64::from(crop.size().height());
    damage
        .iter()
        .filter_map(|rectangle| {
            let left = i64::from(rectangle.origin().x).max(crop_left);
            let top = i64::from(rectangle.origin().y).max(crop_top);
            let right = (i64::from(rectangle.origin().x) + i64::from(rectangle.size().width()))
                .min(crop_right);
            let bottom = (i64::from(rectangle.origin().y) + i64::from(rectangle.size().height()))
                .min(crop_bottom);
            (right > left && bottom > top).then_some((left, top, right, bottom))
        })
        .map(|(left, top, right, bottom)| {
            PhysicalRect::new(
                i32::try_from(left - crop_left)
                    .map_err(|_| CaptureError::invalid_frame("cropped damage x exceeds i32"))?,
                i32::try_from(top - crop_top)
                    .map_err(|_| CaptureError::invalid_frame("cropped damage y exceeds i32"))?,
                u32::try_from(right - left)
                    .map_err(|_| CaptureError::invalid_frame("cropped damage width exceeds u32"))?,
                u32::try_from(bottom - top).map_err(|_| {
                    CaptureError::invalid_frame("cropped damage height exceeds u32")
                })?,
            )
        })
        .collect()
}

fn crop_cursor(cursor: &CursorMetadata, crop: PhysicalRect) -> CursorMetadata {
    CursorMetadata {
        position: translate_position(cursor.position, crop),
        hotspot: cursor.hotspot,
        visible: cursor.visible && position_inside(cursor.position, crop),
        shape_id: cursor.shape_id.clone(),
    }
}

fn crop_input_event(event: &InputEvent, crop: PhysicalRect) -> InputEvent {
    match event {
        InputEvent::PointerButton {
            at,
            button,
            state,
            position,
        } => InputEvent::PointerButton {
            at: *at,
            button: *button,
            state: *state,
            position: position
                .filter(|position| position_inside(*position, crop))
                .map(|position| translate_position(position, crop)),
        },
        _ => event.clone(),
    }
}

fn position_inside(position: PhysicalPosition, crop: PhysicalRect) -> bool {
    let x = i64::from(position.x);
    let y = i64::from(position.y);
    let left = i64::from(crop.origin().x);
    let top = i64::from(crop.origin().y);
    x >= left
        && y >= top
        && x < left + i64::from(crop.size().width())
        && y < top + i64::from(crop.size().height())
}

fn translate_position(position: PhysicalPosition, crop: PhysicalRect) -> PhysicalPosition {
    PhysicalPosition {
        x: position.x.saturating_sub(crop.origin().x),
        y: position.y.saturating_sub(crop.origin().y),
    }
}

fn invalid_target(message: impl Into<String>) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::InvalidRequest,
        message,
        RecoveryHint::ChangeRequest,
    )
}

fn invalid_transition(message: impl Into<String>) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::InvalidStateTransition,
        message,
        RecoveryHint::None,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use gif_from_screen_capture::{
        ButtonState, CaptureCadence, CaptureTimestamp, KeyState, PointerButton,
    };

    use super::*;

    #[derive(Default)]
    struct Calls {
        pauses: usize,
        resumes: usize,
        stops: usize,
        discards: usize,
        inner_updates: usize,
    }

    struct FakeSession {
        request: CaptureRequest,
        state: CaptureSessionState,
        frames: VecDeque<CapturedFrame>,
        calls: Arc<Mutex<Calls>>,
    }

    impl CaptureSession for FakeSession {
        fn state(&self) -> CaptureSessionState {
            self.state
        }
        fn request(&self) -> &CaptureRequest {
            &self.request
        }
        fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().inner_updates += 1;
            self.request.target = target;
            Ok(())
        }
        fn pause(&mut self) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().pauses += 1;
            self.state = CaptureSessionState::Paused;
            Ok(())
        }
        fn resume(&mut self) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().resumes += 1;
            self.state = CaptureSessionState::Recording;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().stops += 1;
            self.state = CaptureSessionState::Stopped;
            Ok(())
        }
        fn discard(&mut self) -> Result<(), CaptureError> {
            self.calls.lock().unwrap().discards += 1;
            self.state = CaptureSessionState::Discarded;
            Ok(())
        }
        fn poll_frame(&mut self, _timeout: Duration) -> Result<FramePoll, CaptureError> {
            if self.state == CaptureSessionState::Paused {
                return Ok(FramePoll::Pending);
            }
            Ok(self
                .frames
                .pop_front()
                .map_or(FramePoll::Pending, FramePoll::Frame))
        }
    }

    fn source_id() -> CaptureSourceId {
        CaptureSourceId::new("wayland:portal:monitor").unwrap()
    }

    fn source_frame(format: PixelFormat) -> CapturedFrame {
        let rgba = [
            [1, 2, 3, 4],
            [5, 6, 7, 8],
            [9, 10, 11, 12],
            [13, 14, 15, 16],
            [17, 18, 19, 20],
            [21, 22, 23, 24],
        ];
        let mut pixels = Vec::new();
        for row in rgba.chunks(3) {
            for pixel in row {
                match format {
                    PixelFormat::Rgba8 => pixels.extend_from_slice(pixel),
                    PixelFormat::Bgra8 => {
                        pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    }
                    _ => unreachable!(),
                }
            }
            pixels.extend_from_slice(&[99, 99, 99, 99]);
        }
        CapturedFrame::new(
            7,
            CaptureTimestamp::from_micros(123),
            PhysicalSize::new(3, 2).unwrap(),
            16,
            format,
            pixels,
        )
        .unwrap()
    }

    fn wrapped(format: PixelFormat) -> (FixedCropSession, Arc<Mutex<Calls>>) {
        let source = source_id();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let request = CaptureRequest::new(
            CaptureTarget::Monitor(source.clone()),
            CaptureCadence::fixed_fps(30).unwrap(),
        );
        let inner = FakeSession {
            request,
            state: CaptureSessionState::Paused,
            frames: VecDeque::from([source_frame(format)]),
            calls: calls.clone(),
        };
        (
            FixedCropSession::new(
                Box::new(inner),
                source,
                PhysicalSize::new(3, 2).unwrap(),
                PhysicalRect::new(1, 0, 2, 2).unwrap(),
            )
            .unwrap(),
            calls,
        )
    }

    #[test]
    fn crops_padded_rgba_and_bgra_without_changing_timing_identity() {
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
            let (mut session, _) = wrapped(format);
            session.resume().unwrap();
            let FramePoll::Frame(frame) = session.poll_frame(Duration::ZERO).unwrap() else {
                panic!("expected cropped frame");
            };
            assert_eq!(frame.sequence(), 7);
            assert_eq!(frame.captured_at().as_micros(), 123);
            assert_eq!(frame.size(), PhysicalSize::new(2, 2).unwrap());
            assert_eq!(frame.stride(), 8);
            assert_eq!(
                frame.pixels(),
                &[5, 6, 7, 8, 9, 10, 11, 12, 17, 18, 19, 20, 21, 22, 23, 24]
            );
        }
    }

    #[test]
    fn crop_translates_damage_cursor_and_pointer_events_without_losing_keys() {
        let source = source_frame(PixelFormat::Rgba8)
            .with_damage(vec![
                PhysicalRect::new(0, 0, 2, 1).unwrap(),
                PhysicalRect::new(2, 1, 1, 1).unwrap(),
            ])
            .unwrap()
            .with_cursor(CursorMetadata {
                position: PhysicalPosition { x: 2, y: 1 },
                hotspot: PhysicalPosition { x: 3, y: 4 },
                visible: true,
                shape_id: Some("cursor-1".to_owned()),
            })
            .with_input_events(vec![
                InputEvent::Key {
                    at: CaptureTimestamp::from_micros(100),
                    native_code: 42,
                    text: Some("x".to_owned()),
                    state: KeyState::Pressed,
                },
                InputEvent::PointerButton {
                    at: CaptureTimestamp::from_micros(110),
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                    position: Some(PhysicalPosition { x: 2, y: 0 }),
                },
                InputEvent::PointerButton {
                    at: CaptureTimestamp::from_micros(120),
                    button: PointerButton::Secondary,
                    state: ButtonState::Released,
                    position: Some(PhysicalPosition { x: 0, y: 0 }),
                },
            ]);
        let crop = PhysicalRect::new(1, 0, 2, 2).unwrap();
        let cropped = crop_frame(&source, source.size(), crop).unwrap();

        assert_eq!(
            cropped.damage(),
            &[
                PhysicalRect::new(0, 0, 1, 1).unwrap(),
                PhysicalRect::new(1, 1, 1, 1).unwrap(),
            ]
        );
        assert_eq!(
            cropped.cursor(),
            Some(&CursorMetadata {
                position: PhysicalPosition { x: 1, y: 1 },
                hotspot: PhysicalPosition { x: 3, y: 4 },
                visible: true,
                shape_id: Some("cursor-1".to_owned()),
            })
        );
        assert!(matches!(
            cropped.input_events()[0],
            InputEvent::Key {
                native_code: 42,
                ..
            }
        ));
        assert!(matches!(
            cropped.input_events()[1],
            InputEvent::PointerButton {
                position: Some(PhysicalPosition { x: 1, y: 0 }),
                ..
            }
        ));
        assert!(matches!(
            cropped.input_events()[2],
            InputEvent::PointerButton { position: None, .. }
        ));

        let outside = source_frame(PixelFormat::Rgba8).with_cursor(CursorMetadata {
            position: PhysicalPosition { x: 0, y: 0 },
            hotspot: PhysicalPosition { x: 0, y: 0 },
            visible: true,
            shape_id: None,
        });
        let outside = crop_frame(&outside, outside.size(), crop).unwrap();
        let cursor = outside.cursor().unwrap();
        assert_eq!(cursor.position, PhysicalPosition { x: -1, y: 0 });
        assert!(!cursor.visible);
    }

    #[test]
    fn retarget_moves_only_the_fixed_canvas_and_never_updates_full_source() {
        let (mut session, calls) = wrapped(PixelFormat::Rgba8);
        let moved = CaptureTarget::Region {
            source: source_id(),
            region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
        };
        session.update_target(moved.clone()).unwrap();
        assert_eq!(session.request().target, moved);
        assert_eq!(session.crop, PhysicalRect::new(0, 0, 2, 2).unwrap());
        assert_eq!(session.source_size, PhysicalSize::new(3, 2).unwrap());
        assert_eq!(calls.lock().unwrap().inner_updates, 0);

        let before = session.request().target.clone();
        for invalid in [
            CaptureTarget::Region {
                source: source_id(),
                region: PhysicalRect::new(0, 0, 1, 2).unwrap(),
            },
            CaptureTarget::Region {
                source: source_id(),
                region: PhysicalRect::new(2, 0, 2, 2).unwrap(),
            },
            CaptureTarget::Region {
                source: CaptureSourceId::new("other").unwrap(),
                region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
            },
        ] {
            assert!(session.update_target(invalid).is_err());
            assert_eq!(session.request().target, before);
        }
    }

    #[test]
    fn lifecycle_delegates_and_terminal_retarget_is_rejected() {
        let (mut session, calls) = wrapped(PixelFormat::Rgba8);
        session.resume().unwrap();
        session.pause().unwrap();
        session.resume().unwrap();
        session.stop().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Stopped);
        assert_eq!(
            session
                .update_target(CaptureTarget::Region {
                    source: source_id(),
                    region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
                })
                .unwrap_err()
                .kind(),
            CaptureErrorKind::InvalidStateTransition
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.resumes, 2);
        assert_eq!(calls.pauses, 1);
        assert_eq!(calls.stops, 1);

        drop(calls);
        let (mut discarded, calls) = wrapped(PixelFormat::Rgba8);
        discarded.discard().unwrap();
        assert_eq!(discarded.state(), CaptureSessionState::Discarded);
        assert_eq!(calls.lock().unwrap().discards, 1);
    }

    #[test]
    fn constructor_rejects_running_session_and_outside_crop() {
        let source = source_id();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let request = CaptureRequest::new(
            CaptureTarget::Monitor(source.clone()),
            CaptureCadence::Manual,
        );
        let make = |state| FakeSession {
            request: request.clone(),
            state,
            frames: VecDeque::new(),
            calls: calls.clone(),
        };
        assert_eq!(
            FixedCropSession::new(
                Box::new(make(CaptureSessionState::Recording)),
                source.clone(),
                PhysicalSize::new(3, 2).unwrap(),
                PhysicalRect::new(0, 0, 2, 2).unwrap(),
            )
            .unwrap_err()
            .kind(),
            CaptureErrorKind::InvalidStateTransition
        );
        assert_eq!(
            FixedCropSession::new(
                Box::new(make(CaptureSessionState::Paused)),
                source,
                PhysicalSize::new(3, 2).unwrap(),
                PhysicalRect::new(2, 0, 2, 2).unwrap(),
            )
            .unwrap_err()
            .kind(),
            CaptureErrorKind::InvalidRequest
        );
    }
}
