//! Bounded write-behind for screen recording. Only the recording worker waits
//! for barriers/shutdown; the UI and native input path never join this thread.

use std::{
    collections::VecDeque,
    io,
    mem::size_of,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use gif_from_screen_capture::InputEvent;
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::ActiveProject;
use gif_from_screen_workflow::{RecordingFrameSink, RecordingFrameSinkError, RecordingMetadata};

use super::IncrementalProjectFrameSink;

const MAX_PENDING_BYTES: usize = 96 * 1024 * 1024;
const MAX_PENDING_COMMANDS: usize = 64;

enum Command {
    Append {
        index: u64,
        frame: RgbaFrame,
        metadata: Option<RecordingMetadata>,
    },
    Duration {
        index: u64,
        duration_us: u64,
    },
    Barrier(mpsc::SyncSender<Result<(), String>>),
}

struct Pending {
    command: Command,
    bytes: usize,
}

#[derive(Clone, Copy)]
enum Close {
    Finish,
    Preserve,
    Discard,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Pending>,
    // Includes the command currently being written, not just queued frames.
    bytes: usize,
    commands: usize,
    close: Option<Close>,
    failure: Option<String>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// Admission is bounded and nonblocking. The durable `RecordingFrameSink`
/// methods additionally wait for a barrier; enqueue methods only accept work.
pub(super) struct BufferedRecordingSink {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<Result<Option<ActiveProject>, String>>>,
    root: PathBuf,
    byte_limit: usize,
}

impl BufferedRecordingSink {
    pub(super) fn new(project: super::IncrementalRecordingProject) -> Result<Self, String> {
        let root = project.root().to_path_buf();
        Self::spawn(
            IncrementalProjectFrameSink::new(project),
            |sink| sink.finish().map_err(|error| error.to_string()),
            root,
            MAX_PENDING_BYTES,
        )
    }

    fn spawn<T, F>(target: T, finish: F, root: PathBuf, byte_limit: usize) -> Result<Self, String>
    where
        T: RecordingFrameSink + Send + 'static,
        F: FnOnce(T) -> Result<ActiveProject, String> + Send + 'static,
    {
        let shared = Arc::new(Shared::default());
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("gfs-record-write".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_writer(target, finish, &worker_shared)
                }))
                .unwrap_or_else(|_| {
                    Err("recording writer panicked; recover the autosaved project".to_owned())
                });
                if let Err(error) = &result {
                    let mut state = worker_shared
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.failure = Some(error.clone());
                    state.pending.clear();
                    state.bytes = 0;
                    state.commands = 0;
                }
                result
            })
            .map_err(|error| format!("could not start recording writer: {error}"))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            root,
            byte_limit,
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn finish(mut self) -> Result<ActiveProject, String> {
        self.close(Close::Finish);
        self.join()?
            .ok_or_else(|| "recording writer ended without a finalized project".to_owned())
    }

    /// Stops accepting work and drains every previously accepted command before
    /// releasing the project lock. The caller can then report/reopen recovery.
    pub(super) fn preserve(mut self) -> Result<(), String> {
        self.close(Close::Preserve);
        self.join().map(|_| ())
    }

    /// Waits for an in-flight durable operation, but does not write queued work
    /// the user explicitly discarded. No writer can access the directory after
    /// this returns, so cleanup cannot race with a late frame write.
    pub(super) fn discard(mut self) -> Result<(), String> {
        self.close(Close::Discard);
        self.join().map(|_| ())
    }

    fn close(&self, mode: Close) {
        let mut state = self.shared.state.lock().unwrap();
        if state.close.is_none() {
            state.close = Some(mode);
        }
        if matches!(state.close, Some(Close::Discard)) {
            while let Some(pending) = state.pending.pop_front() {
                state.bytes -= pending.bytes;
                state.commands -= 1;
            }
        }
        self.shared.wake.notify_one();
    }

    fn join(&mut self) -> Result<Option<ActiveProject>, String> {
        self.worker.take().map_or(Ok(None), |worker| {
            worker.join().map_err(|_| {
                "recording writer panicked; recover the autosaved project".to_owned()
            })?
        })
    }

    fn enqueue(
        &self,
        bytes: usize,
        make_command: impl FnOnce() -> Command,
    ) -> Result<(), RecordingFrameSinkError> {
        let mut state = self.shared.state.lock().unwrap();
        if let Some(error) = &state.failure {
            return Err(io::Error::other(error.clone()).into());
        }
        if state.close.is_some() {
            return Err(io::Error::other("recording writer is already closing").into());
        }
        let next_bytes = state.bytes.checked_add(bytes);
        if next_bytes.is_none_or(|total| total > self.byte_limit)
            || state.commands >= MAX_PENDING_COMMANDS
        {
            return Err(io::Error::other(
                "Recording stopped because storage could not keep up. Open the autosaved project to recover accepted frames.",
            )
            .into());
        }
        // Reserve before cloning any owned metadata/pixels.
        state.bytes = next_bytes.unwrap();
        state.commands += 1;
        state.pending.push_back(Pending {
            command: make_command(),
            bytes,
        });
        self.shared.wake.notify_one();
        Ok(())
    }
}

impl Drop for BufferedRecordingSink {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.close(Close::Preserve);
            let _ = self.join();
        }
    }
}

impl RecordingFrameSink for BufferedRecordingSink {
    fn append_provisional_frame(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        self.enqueue(frame.pixels().len(), || Command::Append {
            index,
            frame: frame.clone(),
            metadata: None,
        })?;
        self.flush_pending()
    }

    fn append_provisional_frame_with_metadata(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
        metadata: &RecordingMetadata,
    ) -> Result<(), RecordingFrameSinkError> {
        self.enqueue_provisional_frame_with_metadata(index, frame, metadata)?;
        self.flush_pending()
    }

    fn enqueue_provisional_frame_with_metadata(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
        metadata: &RecordingMetadata,
    ) -> Result<(), RecordingFrameSinkError> {
        let bytes = frame
            .pixels()
            .len()
            .saturating_add(metadata_bytes(metadata));
        self.enqueue(bytes, || Command::Append {
            index,
            frame: frame.clone(),
            metadata: Some(metadata.clone()),
        })
    }

    fn update_frame_duration(
        &mut self,
        index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        self.enqueue_frame_duration(index, duration_us)?;
        self.flush_pending()
    }

    fn enqueue_frame_duration(
        &mut self,
        index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        self.enqueue(0, || Command::Duration { index, duration_us })
    }

    fn flush_pending(&mut self) -> Result<(), RecordingFrameSinkError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(error) = &state.failure {
                return Err(io::Error::other(error.clone()).into());
            }
            if state.close.is_some() {
                return Err(io::Error::other("recording writer is already closing").into());
            }
            // One reserved control slot: &mut self permits only one barrier at
            // a time, even if the data queue is full. Never overtake data.
            state.commands += 1;
            state.pending.push_back(Pending {
                command: Command::Barrier(sender),
                bytes: 0,
            });
            self.shared.wake.notify_one();
        }
        receiver
            .recv()
            .map_err(|_| io::Error::other("recording writer exited before durable confirmation"))?
            .map_err(|error| io::Error::other(error).into())
    }
}

fn metadata_bytes(metadata: &RecordingMetadata) -> usize {
    let mut bytes = size_of::<RecordingMetadata>().saturating_add(
        metadata
            .input_events
            .len()
            .saturating_mul(size_of::<InputEvent>()),
    );
    for event in &metadata.input_events {
        if let InputEvent::Key {
            text: Some(text), ..
        } = event
        {
            bytes = bytes.saturating_add(text.len());
        }
    }
    if let Some(image) = &metadata.cursor_image {
        bytes = bytes.saturating_add(image.pixels().len());
    }
    if let Some(id) = metadata
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.shape_id.as_ref())
    {
        bytes = bytes.saturating_add(id.len());
    }
    bytes
}

fn run_writer<T, F>(
    mut target: T,
    finish: F,
    shared: &Shared,
) -> Result<Option<ActiveProject>, String>
where
    T: RecordingFrameSink,
    F: FnOnce(T) -> Result<ActiveProject, String>,
{
    loop {
        let pending = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if let Some(pending) = state.pending.pop_front() {
                    break pending;
                }
                if let Some(mode) = state.close {
                    drop(state);
                    return match mode {
                        Close::Finish => finish(target).map(Some),
                        Close::Preserve | Close::Discard => Ok(None),
                    };
                }
                state = shared.wake.wait(state).unwrap();
            }
        };
        let result = match pending.command {
            Command::Append {
                index,
                frame,
                metadata,
            } => match metadata.as_ref() {
                Some(metadata) => {
                    target.append_provisional_frame_with_metadata(index, &frame, metadata)
                }
                None => target.append_provisional_frame(index, &frame),
            },
            Command::Duration { index, duration_us } => {
                target.update_frame_duration(index, duration_us)
            }
            Command::Barrier(reply) => {
                let result = target.flush_pending().map_err(|error| error.to_string());
                let _ = reply.send(result.clone());
                result.map_err(|error| io::Error::other(error).into())
            }
        };
        let mut state = shared.state.lock().unwrap();
        state.bytes -= pending.bytes;
        state.commands -= 1;
        if let Err(error) = result {
            let message = error.to_string();
            state.failure = Some(message.clone());
            // Dropping barrier senders wakes any pending flush. No later
            // command can be acknowledged as durable after a write failure.
            state.pending.clear();
            state.bytes = 0;
            state.commands = 0;
            return Err(message);
        }
    }
}

#[cfg(test)]
mod tests;
