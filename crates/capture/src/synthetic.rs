use std::collections::VecDeque;
use std::time::Duration;

use crate::{
    BackendDescriptor, BackendStatus, CapabilityStatus, CaptureBackend, CaptureCadence,
    CaptureCapabilities, CaptureCapability, CaptureError, CaptureErrorKind, CaptureRequest,
    CaptureSession, CaptureSessionState, CaptureSource, CaptureSourceId, CaptureSourceKind,
    CaptureTarget, CapturedFrame, CursorCaptureMode, FramePoll, PhysicalRect, RecoveryHint,
};

const SYNTHETIC_SOURCE_ID: &str = "synthetic:monitor:0";

/// Deterministic in-memory backend for application and integration tests.
#[derive(Debug, Clone)]
pub struct SyntheticCaptureBackend {
    capabilities: CaptureCapabilities,
    sources: Vec<CaptureSource>,
    frames: Vec<CapturedFrame>,
}

impl SyntheticCaptureBackend {
    /// Creates a backend with one 1920x1080 monitor and all screen metadata
    /// capabilities enabled.
    ///
    /// # Panics
    ///
    /// This can only panic if the compile-time constant synthetic source id,
    /// dimensions, or scale factor cease to satisfy the public constructors.
    pub fn new(frames: Vec<CapturedFrame>) -> Self {
        let available = CapabilityStatus::Available;
        let capabilities = CaptureCapabilities {
            monitor: available.clone(),
            window: available.clone(),
            arbitrary_region: available.clone(),
            cursor_embedded: available.clone(),
            cursor_metadata: available.clone(),
            passive_mouse_buttons: available.clone(),
            passive_keyboard: available.clone(),
            global_shortcuts: available,
            camera: CapabilityStatus::Unavailable(
                "camera capture uses a separate backend contract".to_owned(),
            ),
        };
        let geometry = PhysicalRect::new(0, 0, 1920, 1080).expect("constant geometry is valid");
        let source = CaptureSource::new(
            CaptureSourceId::new(SYNTHETIC_SOURCE_ID).expect("constant id is valid"),
            "Synthetic monitor",
            CaptureSourceKind::Monitor,
            Some(geometry),
            1.0,
        )
        .expect("constant source is valid");
        Self {
            capabilities,
            sources: vec![source],
            frames,
        }
    }

    /// Replaces the runtime capabilities returned by this fake backend.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: CaptureCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replaces the sources returned by this fake backend.
    #[must_use]
    pub fn with_sources(mut self, sources: Vec<CaptureSource>) -> Self {
        self.sources = sources;
        self
    }

    fn validate_request(&self, request: &CaptureRequest) -> Result<(), CaptureError> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id() == request.target.source_id())
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorKind::SourceNotFound,
                    format!(
                        "capture source '{}' was not found",
                        request.target.source_id()
                    ),
                    RecoveryHint::ChooseDifferentSource,
                )
            })?;

        match &request.target {
            CaptureTarget::Monitor(_) => {
                self.capabilities
                    .require_ready(CaptureCapability::Monitor)?;
                if source.kind() != CaptureSourceKind::Monitor {
                    return Err(CaptureError::invalid_request(
                        "a monitor target must reference a monitor source",
                    ));
                }
            }
            CaptureTarget::Window(_) => {
                self.capabilities.require_ready(CaptureCapability::Window)?;
                if source.kind() != CaptureSourceKind::Window {
                    return Err(CaptureError::invalid_request(
                        "a window target must reference a window source",
                    ));
                }
            }
            CaptureTarget::Region { region, .. } => {
                self.capabilities
                    .require_ready(CaptureCapability::ArbitraryRegion)?;
                if let Some(source_geometry) = source.geometry()
                    && !region.fits_within(source_geometry.size())
                {
                    return Err(CaptureError::invalid_request(
                        "capture region falls outside its parent source",
                    ));
                }
            }
        }

        match request.cursor {
            CursorCaptureMode::Embedded => self
                .capabilities
                .require_ready(CaptureCapability::CursorEmbedded)?,
            CursorCaptureMode::Metadata => self
                .capabilities
                .require_ready(CaptureCapability::CursorMetadata)?,
            CursorCaptureMode::Hidden | CursorCaptureMode::Automatic => {}
        }
        if request.cadence == CaptureCadence::OnInteraction {
            let mouse_ready = self.capabilities.passive_mouse_buttons.is_ready();
            let keyboard_ready = self.capabilities.passive_keyboard.is_ready();
            if !mouse_ready && !keyboard_ready {
                return Err(CaptureError::new(
                    CaptureErrorKind::UnsupportedCapability,
                    "interaction cadence requires passive mouse or keyboard capture",
                    RecoveryHint::ChangeRequest,
                ));
            }
        }
        Ok(())
    }
}

impl CaptureBackend for SyntheticCaptureBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "synthetic",
            display_name: "Synthetic capture backend",
        }
    }

    fn status(&self) -> BackendStatus {
        BackendStatus::Ready
    }

    fn capabilities(&self) -> CaptureCapabilities {
        self.capabilities.clone()
    }

    fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
        Ok(self.sources.clone())
    }

    fn start_session(
        &self,
        request: CaptureRequest,
    ) -> Result<Box<dyn CaptureSession>, CaptureError> {
        self.validate_request(&request)?;
        validate_frame_order(&self.frames)?;
        Ok(Box::new(SyntheticCaptureSession {
            request,
            state: CaptureSessionState::Recording,
            frames: self.frames.clone().into(),
        }))
    }
}

fn validate_frame_order(frames: &[CapturedFrame]) -> Result<(), CaptureError> {
    for pair in frames.windows(2) {
        if pair[1].sequence() <= pair[0].sequence() {
            return Err(CaptureError::invalid_frame(
                "synthetic frame sequence numbers must be strictly increasing",
            ));
        }
        if pair[1].captured_at() < pair[0].captured_at() {
            return Err(CaptureError::invalid_frame(
                "synthetic frame timestamps must be monotonic",
            ));
        }
    }
    Ok(())
}

/// A deterministic pull-based session produced by [`SyntheticCaptureBackend`].
#[derive(Debug)]
pub struct SyntheticCaptureSession {
    request: CaptureRequest,
    state: CaptureSessionState,
    frames: VecDeque<CapturedFrame>,
}

impl SyntheticCaptureSession {
    fn invalid_transition(&self, command: &str) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::InvalidStateTransition,
            format!(
                "cannot {command} a capture session in {:?} state",
                self.state
            ),
            RecoveryHint::None,
        )
    }
}

impl CaptureSession for SyntheticCaptureSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }

    fn request(&self) -> &CaptureRequest {
        &self.request
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        if self.state != CaptureSessionState::Recording {
            return Err(self.invalid_transition("pause"));
        }
        self.state = CaptureSessionState::Paused;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        if self.state != CaptureSessionState::Paused {
            return Err(self.invalid_transition("resume"));
        }
        self.state = CaptureSessionState::Recording;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        if !matches!(
            self.state,
            CaptureSessionState::Recording | CaptureSessionState::Paused
        ) {
            return Err(self.invalid_transition("stop"));
        }
        self.state = CaptureSessionState::Stopped;
        Ok(())
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        if !matches!(
            self.state,
            CaptureSessionState::Recording
                | CaptureSessionState::Paused
                | CaptureSessionState::Stopped
        ) {
            return Err(self.invalid_transition("discard"));
        }
        self.frames.clear();
        self.state = CaptureSessionState::Discarded;
        Ok(())
    }

    fn poll_frame(&mut self, _timeout: Duration) -> Result<FramePoll, CaptureError> {
        match self.state {
            CaptureSessionState::Recording => {
                if let Some(frame) = self.frames.pop_front() {
                    Ok(FramePoll::Frame(frame))
                } else {
                    self.state = CaptureSessionState::Stopped;
                    Ok(FramePoll::EndOfStream)
                }
            }
            CaptureSessionState::Paused
            | CaptureSessionState::Starting
            | CaptureSessionState::Stopping => Ok(FramePoll::Pending),
            CaptureSessionState::Stopped
            | CaptureSessionState::Discarded
            | CaptureSessionState::Failed => Ok(FramePoll::EndOfStream),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{CaptureTimestamp, PhysicalSize, PixelFormat};

    use super::*;

    fn frame(sequence: u64, micros: u64) -> CapturedFrame {
        let size = PhysicalSize::new(1, 1).unwrap();
        CapturedFrame::new(
            sequence,
            CaptureTimestamp::from_micros(micros),
            size,
            4,
            PixelFormat::Rgba8,
            vec![u8::try_from(sequence).unwrap(), 0, 0, 255],
        )
        .unwrap()
    }

    fn request() -> CaptureRequest {
        CaptureRequest::new(
            CaptureTarget::Monitor(CaptureSourceId::new(SYNTHETIC_SOURCE_ID).unwrap()),
            CaptureCadence::fixed_fps(10).unwrap(),
        )
    }

    #[test]
    fn streams_frames_and_ends_deterministically() {
        let backend = SyntheticCaptureBackend::new(vec![frame(0, 0), frame(1, 100_000)]);
        let mut session = backend.start_session(request()).unwrap();
        assert!(matches!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::Frame(frame) if frame.sequence() == 0
        ));
        assert!(matches!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::Frame(frame) if frame.sequence() == 1
        ));
        assert_eq!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::EndOfStream
        );
        assert_eq!(session.state(), CaptureSessionState::Stopped);
    }

    #[test]
    fn pause_resume_and_discard_follow_state_machine() {
        let backend = SyntheticCaptureBackend::new(vec![frame(0, 0)]);
        let mut session = backend.start_session(request()).unwrap();
        session.pause().unwrap();
        assert_eq!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::Pending
        );
        session.resume().unwrap();
        assert!(matches!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::Frame(_)
        ));
        session.discard().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Discarded);
        assert_eq!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::EndOfStream
        );
        assert_eq!(
            session.resume().unwrap_err().kind(),
            CaptureErrorKind::InvalidStateTransition
        );
    }

    #[test]
    fn rejects_non_monotonic_synthetic_frames() {
        let backend = SyntheticCaptureBackend::new(vec![frame(2, 0), frame(1, 1)]);
        let error = backend
            .start_session(request())
            .err()
            .expect("invalid frame order must fail");
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }

    #[test]
    fn reports_missing_source() {
        let backend = SyntheticCaptureBackend::new(Vec::new());
        let missing_request = CaptureRequest::new(
            CaptureTarget::Monitor(CaptureSourceId::new("missing").unwrap()),
            CaptureCadence::Manual,
        );
        let error = backend
            .start_session(missing_request)
            .err()
            .expect("missing source must fail");
        assert_eq!(error.kind(), CaptureErrorKind::SourceNotFound);
    }
}
