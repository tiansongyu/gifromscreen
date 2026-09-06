//! Bounded pipe readers and cancellation-safe child ownership.

use std::{
    io::{self, Read},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use super::VideoImportError;

const STDERR_LIMIT: usize = 8192;
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy)]
pub(crate) enum OutputMode {
    Probe { limit: usize },
    Frames { bytes: usize, limit: usize },
}

/// A child must be killed and reaped even if a consumer callback panics.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn run(
    command: Command,
    program: &'static str,
    mode: OutputMode,
    cancelled: &AtomicBool,
    timeout: Duration,
    consume: impl FnMut(Vec<u8>) -> Result<(), VideoImportError>,
) -> Result<(), VideoImportError> {
    run_with_check(
        command,
        program,
        mode,
        || cancelled.load(Ordering::Relaxed),
        timeout,
        consume,
    )
}

pub(crate) fn run_with_check(
    mut command: Command,
    program: &'static str,
    mode: OutputMode,
    is_cancelled: impl Fn() -> bool,
    timeout: Duration,
    mut consume: impl FnMut(Vec<u8>) -> Result<(), VideoImportError>,
) -> Result<(), VideoImportError> {
    if is_cancelled() {
        return Err(VideoImportError::Cancelled);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| VideoImportError::StartProcess { program, source })?;
    let mut child = OwnedChild(child);
    let stdout = child.0.stdout.take().expect("stdout was piped");
    let stderr = child.0.stderr.take().expect("stderr was piped");
    // A rendezvous channel permits only the consumer's frame and the reader's
    // next frame. No unbounded frame queue or complete decoded movie is kept.
    let (sender, receiver) = mpsc::sync_channel(0);
    let output_thread = thread::spawn(move || read_output(stdout, mode, &sender));
    let stderr_thread = thread::spawn(move || read_tail(stderr, STDERR_LIMIT));
    let started = Instant::now();
    let mut output_closed = false;
    let result = loop {
        if is_cancelled() {
            break Err(VideoImportError::Cancelled);
        }
        if started.elapsed() >= timeout {
            break Err(VideoImportError::Timeout { program });
        }
        if output_closed {
            match child.0.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(source) => {
                    break Err(VideoImportError::Io {
                        operation: "check video decoder process",
                        source,
                    });
                }
            }
            continue;
        }
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(Ok(bytes)) => {
                if let Err(error) = consume(bytes) {
                    break Err(error);
                }
            }
            Ok(Err(error)) => break Err(error),
            Err(RecvTimeoutError::Disconnected) => output_closed = true,
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    // Drop the receiver before joining: the reader may be waiting to deliver a
    // frame when cancellation or a persistence error stops the consumer.
    drop(receiver);
    if result.is_err() {
        let _ = child.0.kill();
    }
    let wait = child.0.wait();
    let output_join = output_thread.join();
    let stderr = stderr_thread.join().unwrap_or_default();
    let status = result?;
    wait.map_err(|source| VideoImportError::Io {
        operation: "reap video decoder process",
        source,
    })?;
    if output_join.is_err() {
        return Err(VideoImportError::InvalidVideo(
            "video output reader stopped unexpectedly".to_owned(),
        ));
    }
    if !status.success() {
        return Err(VideoImportError::ProcessFailed {
            program,
            detail: String::from_utf8_lossy(&stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn read_output(
    mut stdout: ChildStdout,
    mode: OutputMode,
    sender: &SyncSender<Result<Vec<u8>, VideoImportError>>,
) {
    match mode {
        OutputMode::Probe { limit } => {
            let mut output = Vec::new();
            let result = (&mut stdout)
                .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
                .read_to_end(&mut output)
                .map_err(|source| VideoImportError::Io {
                    operation: "read video metadata",
                    source,
                })
                .and_then(|_| {
                    if output.len() > limit {
                        Err(VideoImportError::InvalidVideo(
                            "video metadata exceeded the 64 KiB safety limit".to_owned(),
                        ))
                    } else {
                        Ok(output)
                    }
                });
            let _ = sender.send(result);
        }
        OutputMode::Frames { bytes, limit } => {
            for index in 0..=limit {
                let mut frame = Vec::new();
                if frame.try_reserve_exact(bytes).is_err() {
                    let _ = sender.send(Err(VideoImportError::InvalidVideo(
                        "could not allocate the bounded video frame".to_owned(),
                    )));
                    return;
                }
                frame.resize(bytes, 0);
                match read_frame(&mut stdout, &mut frame) {
                    Ok(false) => return,
                    Ok(true) if index == limit => {
                        let _ = sender.send(Err(VideoImportError::InvalidVideo(
                            "decoder produced more than the requested frame limit".to_owned(),
                        )));
                        return;
                    }
                    Ok(true) => {
                        if sender.send(Ok(frame)).is_err() {
                            return;
                        }
                    }
                    Err(source) => {
                        let _ = sender.send(Err(VideoImportError::Io {
                            operation: "read complete RGBA video frame",
                            source,
                        }));
                        return;
                    }
                }
            }
        }
    }
}

fn read_frame(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < bytes.len() {
        match reader.read(&mut bytes[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn read_tail(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut tail = Vec::with_capacity(limit);
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let discard = tail.len().saturating_add(count).saturating_sub(limit);
        tail.drain(..discard.min(tail.len()));
        let start = count.saturating_sub(limit);
        tail.extend_from_slice(&buffer[start..count]);
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reader_distinguishes_clean_eof_from_partial_frame() {
        let mut output = [0; 4];
        assert!(!read_frame(&mut &[][..], &mut output).unwrap());
        assert!(read_frame(&mut &[1, 2, 3, 4][..], &mut output).unwrap());
        assert_eq!(output, [1, 2, 3, 4]);
        assert_eq!(
            read_frame(&mut &[1, 2, 3][..], &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn diagnostic_tail_keeps_only_the_last_bounded_bytes() {
        assert_eq!(read_tail(&b"0123456789"[..], 4), b"6789");
        let bytes = vec![b'a'; 100_000];
        assert_eq!(read_tail(bytes.as_slice(), 8192).len(), 8192);
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_stalled_process_is_killed_and_reaped_promptly() {
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(80));
                cancelled.store(true, Ordering::Relaxed);
            });
            let started = Instant::now();
            let error = run(
                shell("exec sleep 30"),
                "fake decoder",
                OutputMode::Frames { bytes: 4, limit: 2 },
                &cancelled,
                Duration::from_secs(5),
                |_| Ok(()),
            )
            .unwrap_err();
            assert!(error.is_cancelled());
            assert!(started.elapsed() < Duration::from_secs(2));
        });
    }

    #[cfg(unix)]
    #[test]
    fn closed_stdout_does_not_bypass_process_timeout() {
        let started = Instant::now();
        let error = run(
            shell("exec 1>&-; exec sleep 30"),
            "fake decoder",
            OutputMode::Frames { bytes: 4, limit: 2 },
            &AtomicBool::new(false),
            Duration::from_millis(80),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, VideoImportError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn stderr_flood_is_drained_without_blocking_stdout() {
        let mut frames = 0;
        run(
            shell("i=0; while [ $i -lt 10000 ]; do printf 'diagnostics diagnostics diagnostics\n' >&2; i=$((i+1)); done; printf abcd"),
            "fake decoder",
            OutputMode::Frames { bytes: 4, limit: 2 },
            &AtomicBool::new(false),
            Duration::from_secs(10),
            |bytes| {
                assert_eq!(bytes, b"abcd");
                frames += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(frames, 1);
    }

    #[cfg(unix)]
    #[test]
    fn excess_probe_output_terminates_a_stalled_producer() {
        let error = run(
            shell("printf 123456789; exec sleep 30"),
            "fake probe",
            OutputMode::Probe { limit: 8 },
            &AtomicBool::new(false),
            Duration::from_secs(2),
            |_| panic!("oversize metadata must not be delivered"),
        )
        .unwrap_err();
        assert!(matches!(error, VideoImportError::InvalidVideo(_)));
    }

    #[cfg(unix)]
    #[test]
    fn consumer_failure_releases_a_blocked_frame_sender() {
        let error = run(
            shell("while :; do printf abcdefgh; done"),
            "fake decoder",
            OutputMode::Frames {
                bytes: 4,
                limit: 10,
            },
            &AtomicBool::new(false),
            Duration::from_secs(2),
            |_| Err(VideoImportError::Cancelled),
        )
        .unwrap_err();
        assert!(error.is_cancelled());
    }

    #[test]
    fn missing_executable_reports_the_required_installation() {
        let directory = tempfile::tempdir().unwrap();
        let error = run(
            Command::new(directory.path().join("missing-ffmpeg")),
            "ffmpeg",
            OutputMode::Frames { bytes: 4, limit: 1 },
            &AtomicBool::new(false),
            Duration::from_secs(1),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, VideoImportError::StartProcess { .. }));
        assert!(
            error
                .to_string()
                .contains("Install the system FFmpeg package")
        );
    }

    #[cfg(unix)]
    #[test]
    fn complete_frames_beyond_limit_are_never_delivered() {
        let mut frames = 0;
        let error = run(
            shell("printf abcdefghijkl"),
            "fake decoder",
            OutputMode::Frames { bytes: 4, limit: 1 },
            &AtomicBool::new(false),
            Duration::from_secs(1),
            |_| {
                frames += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(matches!(error, VideoImportError::InvalidVideo(_)));
        assert_eq!(frames, 1);
    }

    #[cfg(unix)]
    #[test]
    fn unsuccessful_exit_retains_only_bounded_diagnostics() {
        let error = run(
            shell("printf 'invalid codec' >&2; exit 7"),
            "fake decoder",
            OutputMode::Frames { bytes: 4, limit: 1 },
            &AtomicBool::new(false),
            Duration::from_secs(1),
            |_| Ok(()),
        )
        .unwrap_err();
        let VideoImportError::ProcessFailed { detail, .. } = error else {
            panic!("expected decoder failure");
        };
        assert_eq!(detail, "invalid codec");
    }
}
