//! One cancellable worker, one terminal result, and a replaceable progress snapshot.

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

pub(crate) struct TaskContext<P> {
    cancellation: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<P>>>,
}

impl<P> TaskContext<P> {
    pub(crate) fn cancellation(&self) -> &AtomicBool {
        &self.cancellation
    }
    pub(crate) fn report(&self, progress: P) {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = Some(progress);
    }
}

pub(crate) struct BackgroundTask<T, P> {
    result: Option<Receiver<Result<T, String>>>,
    cancellation: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<P>>>,
}

impl<T, P> Default for BackgroundTask<T, P> {
    fn default() -> Self {
        Self {
            result: None,
            cancellation: Arc::default(),
            progress: Arc::default(),
        }
    }
}

impl<T, P> Drop for BackgroundTask<T, P> {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
    }
}

impl<T: Send + 'static, P: Clone + Send + 'static> BackgroundTask<T, P> {
    pub(crate) fn is_running(&self) -> bool {
        self.result.is_some()
    }

    pub(crate) fn start(
        &mut self,
        name: &str,
        worker: impl FnOnce(TaskContext<P>) -> Result<T, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.is_running() {
            return Err("This operation is already running.".to_owned());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(None));
        let context = TaskContext {
            cancellation: Arc::clone(&cancellation),
            progress: Arc::clone(&progress),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _ = sender.send(worker(context));
            })
            .map_err(|error| format!("Could not start worker: {error}"))?;
        self.result = Some(receiver);
        self.cancellation = cancellation;
        self.progress = progress;
        Ok(())
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }
    pub(crate) fn is_cancelling(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
    pub(crate) fn progress(&self) -> Option<P> {
        self.progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn poll(&mut self) -> Option<Result<T, String>> {
        let result = match self.result.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(
                "The worker exited without a result. Any recoverable project remains on disk."
                    .to_owned(),
            ),
        };
        self.result = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn cancellation_keeps_one_worker_slot_until_terminal_result() {
        let mut task = BackgroundTask::<u32, u32>::default();
        task.start("task-test", |context| {
            context.report(4);
            while !context.cancellation().load(Ordering::Acquire) {
                thread::yield_now();
            }
            Ok(9)
        })
        .unwrap();
        task.cancel();
        assert!(task.start("second", |_| Ok(10)).is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = task.poll() {
                assert_eq!(result.unwrap(), 9);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(task.progress(), Some(4));
        assert!(!task.is_running());
        assert!(task.poll().is_none());
    }
}
