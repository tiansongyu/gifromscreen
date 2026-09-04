//! Non-blocking Linux capture-source discovery for the recorder page.

use std::{
    io,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use gif_from_screen_capture::{CaptureError, CaptureSource};
use gif_from_screen_capture_linux::{LinuxCaptureBackend, LinuxDisplayServer};
use thiserror::Error;

const SOURCE_THREAD_NAME: &str = "gfs-linux-capture-sources";

/// Observable lifecycle of asynchronous Linux source discovery.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CaptureSourceJobState {
    /// Discovery has not started, or its previous result was consumed.
    #[default]
    Idle,
    /// Native backend initialization and enumeration are running.
    Loading,
    /// A terminal result is ready to consume.
    Finished,
}

/// Sources returned by one detected Linux display backend.
#[derive(Debug)]
pub(crate) struct CaptureSourceCatalog {
    display_server: LinuxDisplayServer,
    sources: Vec<CaptureSource>,
}

impl CaptureSourceCatalog {
    pub(crate) const fn display_server(&self) -> LinuxDisplayServer {
        self.display_server
    }

    pub(crate) fn into_sources(self) -> Vec<CaptureSource> {
        self.sources
    }
}

/// Typed failures from Linux display detection and native source discovery.
#[derive(Debug, Error)]
pub(crate) enum CaptureSourceJobError {
    #[error("no Linux Wayland or X11 graphical session was detected")]
    NoDisplayServer,
    #[error("could not initialize the detected Linux capture backend: {0}")]
    Initialize(#[source] CaptureError),
    #[error("could not enumerate capture sources: {0}")]
    Enumerate(#[source] CaptureError),
    #[error("the {display_server:?} backend reported no capture sources")]
    Empty { display_server: LinuxDisplayServer },
    #[error("capture-source worker exited without reporting a result")]
    WorkerExited,
}

/// Failure to launch asynchronous source discovery.
#[derive(Debug, Error)]
pub(crate) enum CaptureSourceJobStartError {
    #[error("capture-source discovery is already {state:?}")]
    AlreadyStarted { state: CaptureSourceJobState },
    #[error("could not spawn capture-source worker: {0}")]
    Spawn(#[source] io::Error),
}

/// UI-owned handle whose polling methods never wait on native APIs.
#[derive(Default)]
pub(crate) struct CaptureSourceJob {
    state: CaptureSourceJobState,
    receiver: Option<Receiver<Result<CaptureSourceCatalog, CaptureSourceJobError>>>,
    result: Option<Result<CaptureSourceCatalog, CaptureSourceJobError>>,
}

impl CaptureSourceJob {
    pub(crate) fn start(&mut self) -> Result<(), CaptureSourceJobStartError> {
        self.start_with(load_linux_sources)
    }

    fn start_with<F>(&mut self, loader: F) -> Result<(), CaptureSourceJobStartError>
    where
        F: FnOnce() -> Result<CaptureSourceCatalog, CaptureSourceJobError> + Send + 'static,
    {
        if self.state != CaptureSourceJobState::Idle {
            return Err(CaptureSourceJobStartError::AlreadyStarted { state: self.state });
        }
        self.state = CaptureSourceJobState::Loading;
        self.result = None;
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(SOURCE_THREAD_NAME.to_owned())
            .spawn(move || {
                let _ = sender.send(loader());
            });
        if let Err(error) = spawn {
            self.state = CaptureSourceJobState::Idle;
            return Err(CaptureSourceJobStartError::Spawn(error));
        }
        self.receiver = Some(receiver);
        Ok(())
    }

    pub(crate) const fn state(&self) -> CaptureSourceJobState {
        self.state
    }

    /// Polls the worker exactly once without blocking the UI thread.
    pub(crate) fn drain(&mut self) -> bool {
        if self.state != CaptureSourceJobState::Loading {
            return false;
        }
        let result = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Empty)) => return false,
            Some(Err(TryRecvError::Disconnected)) | None => {
                Err(CaptureSourceJobError::WorkerExited)
            }
        };
        self.receiver = None;
        self.result = Some(result);
        self.state = CaptureSourceJobState::Finished;
        true
    }

    pub(crate) fn take_result(
        &mut self,
    ) -> Option<Result<CaptureSourceCatalog, CaptureSourceJobError>> {
        let result = self.result.take()?;
        self.state = CaptureSourceJobState::Idle;
        Some(result)
    }
}

fn load_linux_sources() -> Result<CaptureSourceCatalog, CaptureSourceJobError> {
    let detected = LinuxCaptureBackend::detect();
    let display_server = detected
        .report()
        .environment
        .display_server()
        .ok_or(CaptureSourceJobError::NoDisplayServer)?;
    let backend = detected
        .initialize_native()
        .map_err(CaptureSourceJobError::Initialize)?;
    let sources = backend
        .list_sources()
        .map_err(CaptureSourceJobError::Enumerate)?;
    if sources.is_empty() {
        return Err(CaptureSourceJobError::Empty { display_server });
    }
    Ok(CaptureSourceCatalog {
        display_server,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use gif_from_screen_capture::{CaptureSourceId, CaptureSourceKind, PhysicalRect};

    use super::*;

    fn wait_for_result(job: &mut CaptureSourceJob) {
        for _ in 0..200 {
            if job.drain() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("source worker did not finish in time");
    }

    #[test]
    fn loader_can_block_without_blocking_ui_polling() {
        let (release, gate) = mpsc::channel();
        let mut job = CaptureSourceJob::default();
        job.start_with(move || {
            gate.recv().unwrap();
            Err(CaptureSourceJobError::NoDisplayServer)
        })
        .unwrap();

        assert_eq!(job.state(), CaptureSourceJobState::Loading);
        assert!(!job.drain());
        release.send(()).unwrap();
        wait_for_result(&mut job);
        assert!(matches!(
            job.take_result(),
            Some(Err(CaptureSourceJobError::NoDisplayServer))
        ));
        assert_eq!(job.state(), CaptureSourceJobState::Idle);
    }

    #[test]
    fn wayland_catalog_accepts_sources_without_geometry() {
        let source = CaptureSource::new(
            CaptureSourceId::new("wayland:portal:monitor").unwrap(),
            "Choose a screen",
            CaptureSourceKind::Monitor,
            None,
            1.0,
        )
        .unwrap();
        let mut job = CaptureSourceJob::default();
        job.start_with(move || {
            Ok(CaptureSourceCatalog {
                display_server: LinuxDisplayServer::Wayland,
                sources: vec![source],
            })
        })
        .unwrap();

        wait_for_result(&mut job);
        let catalog = job.take_result().unwrap().unwrap();
        assert_eq!(catalog.display_server(), LinuxDisplayServer::Wayland);
        assert_eq!(catalog.sources.len(), 1);
        assert!(catalog.sources[0].geometry().is_none());
    }

    #[test]
    fn x11_catalog_keeps_real_source_geometry() {
        let geometry = PhysicalRect::new(-1_920, 0, 1_920, 1_080).unwrap();
        let source = CaptureSource::new(
            CaptureSourceId::new("x11:monitor:0").unwrap(),
            "Left monitor",
            CaptureSourceKind::Monitor,
            Some(geometry),
            1.0,
        )
        .unwrap();
        let catalog = CaptureSourceCatalog {
            display_server: LinuxDisplayServer::X11,
            sources: vec![source],
        };
        assert_eq!(catalog.sources[0].geometry(), Some(geometry));
    }
}
