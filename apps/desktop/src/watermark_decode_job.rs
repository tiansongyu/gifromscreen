use std::{
    fs::{self, File},
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use gif_from_screen_domain::PhysicalSize;
use gif_from_screen_media::{
    DecodeLimits, StaticImageDecodeError, StaticImageDecodeOptions, decode_static_image_with_format,
};
use thiserror::Error;

const THREAD_NAME: &str = "gfs-watermark-decode";
const MAX_EDGE: u16 = 4_096;
const MAX_RGBA_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedWatermark {
    pub(crate) source_path: PathBuf,
    pub(crate) size: PhysicalSize,
    pub(crate) rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WatermarkDecodeJobState {
    #[default]
    Idle,
    Running,
    Finished,
}

#[derive(Debug, Error)]
pub(crate) enum WatermarkDecodeError {
    #[error("watermark image does not exist: {}", path.display())]
    NotFound { path: PathBuf },
    #[error("watermark input is not a regular file: {}", path.display())]
    NotFile { path: PathBuf },
    #[error("watermark must end in PNG, JPG/JPEG, BMP, or WebP: {}", path.display())]
    InvalidExtension { path: PathBuf },
    #[error("could not inspect watermark {}: {source}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not open watermark {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not decode watermark {}: {source}", path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: Box<StaticImageDecodeError>,
    },
    #[error("watermark decoder returned {actual} frames instead of one")]
    FrameCount { actual: usize },
    #[error("watermark dimensions are invalid: {0}")]
    InvalidSize(#[source] gif_from_screen_domain::UnitError),
    #[error("watermark worker exited without a terminal result")]
    WorkerExited,
}

#[derive(Debug, Error)]
pub(crate) enum WatermarkDecodeStartError {
    #[error("watermark decode job is already {state:?}")]
    AlreadyStarted { state: WatermarkDecodeJobState },
    #[error(transparent)]
    InvalidInput(#[from] WatermarkDecodeError),
    #[error("could not spawn watermark decoder: {0}")]
    Spawn(#[source] io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WatermarkDecodeEvent {
    Finished,
}

enum WorkerMessage {
    Finished(Result<DecodedWatermark, WatermarkDecodeError>),
}

#[derive(Default)]
pub(crate) struct WatermarkDecodeJob {
    state: WatermarkDecodeJobState,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<DecodedWatermark, WatermarkDecodeError>>,
}

impl WatermarkDecodeJob {
    pub(crate) fn start(&mut self, path: PathBuf) -> Result<(), WatermarkDecodeStartError> {
        if self.state != WatermarkDecodeJobState::Idle {
            return Err(WatermarkDecodeStartError::AlreadyStarted { state: self.state });
        }
        preflight(&path)?;
        let (sender, receiver) = mpsc::channel();
        self.state = WatermarkDecodeJobState::Running;
        let spawn = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || send_result(&sender, decode(path)));
        if let Err(error) = spawn {
            self.state = WatermarkDecodeJobState::Idle;
            return Err(WatermarkDecodeStartError::Spawn(error));
        }
        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    pub(crate) const fn state(&self) -> WatermarkDecodeJobState {
        self.state
    }

    pub(crate) fn drain(&mut self) -> Vec<WatermarkDecodeEvent> {
        if self.state != WatermarkDecodeJobState::Running {
            return Vec::new();
        }
        match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(WorkerMessage::Finished(result))) => {
                self.finish(result);
                vec![WatermarkDecodeEvent::Finished]
            }
            Some(Err(TryRecvError::Empty)) => Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(WatermarkDecodeError::WorkerExited));
                vec![WatermarkDecodeEvent::Finished]
            }
        }
    }

    pub(crate) fn take_result(&mut self) -> Option<Result<DecodedWatermark, WatermarkDecodeError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<DecodedWatermark, WatermarkDecodeError>) {
        self.receiver = None;
        self.result = Some(result);
        self.state = WatermarkDecodeJobState::Finished;
    }
}

fn preflight(path: &Path) -> Result<(), WatermarkDecodeError> {
    let metadata = fs::metadata(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            WatermarkDecodeError::NotFound {
                path: path.to_owned(),
            }
        } else {
            WatermarkDecodeError::Inspect {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(WatermarkDecodeError::NotFile {
            path: path.to_owned(),
        });
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "bmp", "webp"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
    {
        return Err(WatermarkDecodeError::InvalidExtension {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn decode(path: PathBuf) -> Result<DecodedWatermark, WatermarkDecodeError> {
    let file = File::open(&path).map_err(|source| WatermarkDecodeError::Open {
        path: path.clone(),
        source,
    })?;
    let decoded = decode_static_image_with_format(
        BufReader::new(file),
        &StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_width: MAX_EDGE,
                max_height: MAX_EDGE,
                max_frames: 1,
                max_total_rgba_bytes: MAX_RGBA_BYTES,
            },
            ..StaticImageDecodeOptions::default()
        },
    )
    .map_err(|source| WatermarkDecodeError::Decode {
        path: path.clone(),
        source: Box::new(source),
    })?;
    let animation = decoded.into_animation();
    let size = PhysicalSize::new(u32::from(animation.width()), u32::from(animation.height()))
        .map_err(WatermarkDecodeError::InvalidSize)?;
    let mut frames = animation.into_frames();
    if frames.len() != 1 {
        return Err(WatermarkDecodeError::FrameCount {
            actual: frames.len(),
        });
    }
    let rgba = frames
        .pop()
        .expect("the exact one-frame length was checked")
        .into_rgba();
    Ok(DecodedWatermark {
        source_path: path,
        size,
        rgba,
    })
}

fn send_result(
    sender: &Sender<WorkerMessage>,
    result: Result<DecodedWatermark, WatermarkDecodeError>,
) {
    let _ = sender.send(WorkerMessage::Finished(result));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::*;

    const PNG_ALPHA: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 1, 3,
        0, 0, 0, 206, 236, 237, 201, 0, 0, 0, 6, 80, 76, 84, 69, 0, 255, 0, 255, 0, 0, 209, 155,
        74, 174, 0, 0, 0, 1, 116, 82, 78, 83, 64, 54, 58, 153, 246, 0, 0, 0, 10, 73, 68, 65, 84, 8,
        215, 99, 104, 0, 0, 0, 130, 0, 129, 221, 67, 106, 244, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ];

    fn wait(job: &mut WatermarkDecodeJob) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.state() != WatermarkDecodeJobState::Finished {
            let _ = job.drain();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn decodes_one_bounded_rgba_watermark_off_thread() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("logo.png");
        fs::write(&path, PNG_ALPHA).unwrap();
        let mut job = WatermarkDecodeJob::default();
        job.start(path.clone()).unwrap();
        wait(&mut job);
        let decoded = job.take_result().unwrap().unwrap();
        assert_eq!(decoded.source_path, path);
        assert_eq!(decoded.size, PhysicalSize::new(2, 1).unwrap());
        assert_eq!(decoded.rgba.len(), 8);
    }

    #[test]
    fn preflight_and_decode_failures_are_retryable_with_a_fresh_job() {
        let directory = tempdir().unwrap();
        let mut job = WatermarkDecodeJob::default();
        assert!(matches!(
            job.start(directory.path().join("missing.png")),
            Err(WatermarkDecodeStartError::InvalidInput(
                WatermarkDecodeError::NotFound { .. }
            ))
        ));
        assert_eq!(job.state(), WatermarkDecodeJobState::Idle);

        let malformed = directory.path().join("bad.webp");
        fs::write(&malformed, b"not an image").unwrap();
        job.start(malformed).unwrap();
        wait(&mut job);
        assert!(matches!(
            job.take_result(),
            Some(Err(WatermarkDecodeError::Decode { .. }))
        ));
    }
}
