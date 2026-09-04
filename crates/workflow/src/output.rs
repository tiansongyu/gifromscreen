use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use gif_from_screen_capture::{CaptureBackend, CaptureRequest};
use gif_from_screen_gif::{
    BuiltinGifEncoder, CancellationToken, EncodeOptions, EncodeProgress, EncodeReport, GifEncoder,
    IteratorFrameSource, ProgressSink,
};

use crate::{
    CollectOptions, CollectionSummary, WorkflowError, WorkflowPhase, WorkflowProgress,
    WorkflowProgressSink, collect,
};

/// Collection and encoding configuration for [`record_to_gif`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordToGifOptions {
    /// Capture collection limits and timing policy.
    pub collection: CollectOptions,
    /// GIF palette, timing, loop, transparency, and optimization policy.
    pub encoding: EncodeOptions,
}

/// Metadata returned after an atomically committed GIF export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordToGifReport {
    /// Capture collection summary.
    pub collection: CollectionSummary,
    /// GIF encoder report.
    pub encoding: EncodeReport,
    /// Final committed output path.
    pub output_path: PathBuf,
    /// Size of the synchronized temporary file immediately before commit.
    pub bytes_written: u64,
}

/// Returns the sibling temporary path used for an output target.
///
/// `recording.gif` maps to `recording.gif.partial`, keeping the temporary file
/// on the same filesystem so the final rename is atomic on Linux and macOS.
///
/// # Errors
///
/// Returns [`WorkflowError::InvalidOutputPath`] when `target` has no file name.
pub fn partial_output_path(target: impl AsRef<Path>) -> Result<PathBuf, WorkflowError> {
    let target = target.as_ref();
    let file_name = target
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| WorkflowError::InvalidOutputPath(target.to_path_buf()))?;
    let mut partial_name = OsString::from(file_name);
    partial_name.push(".partial");
    Ok(target.with_file_name(partial_name))
}

/// Captures frames and atomically exports them with the built-in GIF encoder.
///
/// This is a synchronous convenience wrapper around
/// [`record_to_gif_with_encoder`]. It is safe to invoke from an
/// application-owned background thread.
///
/// # Errors
///
/// Returns [`WorkflowError`] when collection, encoding, filesystem sync, or
/// atomic commit fails. Temporary `.partial` output is removed on every
/// recoverable failure path.
pub fn record_to_gif(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    target: impl AsRef<Path>,
    options: &RecordToGifOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<RecordToGifReport, WorkflowError> {
    record_to_gif_with_encoder(
        backend,
        &BuiltinGifEncoder::default(),
        request,
        target,
        options,
        cancellation,
        progress,
    )
}

/// Captures frames and atomically exports them with a supplied GIF encoder.
///
/// The target is never written directly. Encoding occurs in a sibling
/// `<target>.partial` file, which is flushed and synchronized before one rename
/// commits it to `target`. An existing target is replaced only after encoding
/// succeeds.
///
/// # Errors
///
/// Returns [`WorkflowError`] when collection, encoding, filesystem sync, or
/// atomic commit fails. If removing a failed temporary output also fails, the
/// returned [`WorkflowError::PartialCleanup`] retains both failures.
#[allow(clippy::too_many_arguments)]
pub fn record_to_gif_with_encoder(
    backend: &dyn CaptureBackend,
    encoder: &dyn GifEncoder,
    request: CaptureRequest,
    target: impl AsRef<Path>,
    options: &RecordToGifOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<RecordToGifReport, WorkflowError> {
    let target = target.as_ref().to_path_buf();
    let partial = partial_output_path(&target)?;
    let recording = collect(
        backend,
        request,
        &options.collection,
        cancellation,
        progress,
    )?;
    let collection = recording.summary();

    if cancellation.is_cancelled() {
        return Err(WorkflowError::Cancelled);
    }

    let encode_result = encode_partial(
        encoder,
        recording.into_frames(),
        &partial,
        &options.encoding,
        collection,
        cancellation,
        progress,
    );
    let (encoding, bytes_written) = match encode_result {
        Ok(report) => report,
        Err(error) => return Err(cleanup_after_failure(&partial, error)),
    };

    progress.report(WorkflowProgress::capture(
        WorkflowPhase::Committing,
        collection.frames,
        std::time::Duration::from_micros(collection.duration_us),
    ));
    if let Err(source) = fs::rename(&partial, &target) {
        let error = WorkflowError::output_io("atomically commit output", &target, source);
        return Err(cleanup_after_failure(&partial, error));
    }
    progress.report(WorkflowProgress::capture(
        WorkflowPhase::Complete,
        collection.frames,
        std::time::Duration::from_micros(collection.duration_us),
    ));
    Ok(RecordToGifReport {
        collection,
        encoding,
        output_path: target,
        bytes_written,
    })
}

fn encode_partial(
    encoder: &dyn GifEncoder,
    frames: Vec<gif_from_screen_gif::RgbaFrame>,
    partial: &Path,
    options: &EncodeOptions,
    collection: CollectionSummary,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<(EncodeReport, u64), WorkflowError> {
    let mut file = File::create(partial)
        .map_err(|source| WorkflowError::output_io("create temporary output", partial, source))?;
    let mut source = IteratorFrameSource::new(frames.into_iter());
    let mut encode_progress = EncodeProgressAdapter {
        sink: progress,
        collection,
    };
    let report = encoder.encode(
        &mut source,
        &mut file,
        options,
        cancellation,
        &mut encode_progress,
    )?;
    file.flush()
        .map_err(|source| WorkflowError::output_io("flush temporary output", partial, source))?;
    file.sync_all().map_err(|source| {
        WorkflowError::output_io("synchronize temporary output", partial, source)
    })?;
    let bytes_written = file
        .metadata()
        .map_err(|source| WorkflowError::output_io("inspect temporary output", partial, source))?
        .len();
    drop(file);
    Ok((report, bytes_written))
}

struct EncodeProgressAdapter<'a> {
    sink: &'a mut dyn WorkflowProgressSink,
    collection: CollectionSummary,
}

impl ProgressSink for EncodeProgressAdapter<'_> {
    fn report(&mut self, progress: EncodeProgress) {
        self.sink.report(WorkflowProgress::encoding(
            self.collection.frames,
            std::time::Duration::from_micros(self.collection.duration_us),
            progress,
        ));
    }
}

fn cleanup_after_failure(partial: &Path, original: WorkflowError) -> WorkflowError {
    match fs::remove_file(partial) {
        Ok(()) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => original,
        Err(cleanup) => WorkflowError::PartialCleanup {
            path: partial.to_path_buf(),
            original: Box::new(original),
            cleanup,
        },
    }
}
