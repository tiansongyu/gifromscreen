use crate::{FrameSourceError, RgbaFrame};

/// Pull-based RGBA stream boundary used by [`crate::GifEncoder`].
///
/// Implementations may decode frames lazily or read them from a project store.
/// They should return an exact total in `frame_count_hint` when inexpensive so
/// the UI can show determinate progress.
pub trait RgbaFrameSource: Send {
    fn next_frame(&mut self) -> Result<Option<RgbaFrame>, FrameSourceError>;

    fn frame_count_hint(&self) -> Option<u64> {
        None
    }
}

/// Infallible adapter for an iterator of owned frames.
pub struct IteratorFrameSource<I> {
    frames: I,
    total_frames: Option<u64>,
}

impl<I> IteratorFrameSource<I>
where
    I: Iterator<Item = RgbaFrame>,
{
    pub fn new(frames: I) -> Self {
        let (lower, upper) = frames.size_hint();
        let total_frames = upper
            .filter(|upper| *upper == lower)
            .and_then(|exact| u64::try_from(exact).ok());
        Self {
            frames,
            total_frames,
        }
    }
}

impl<I> RgbaFrameSource for IteratorFrameSource<I>
where
    I: Iterator<Item = RgbaFrame> + Send,
{
    fn next_frame(&mut self) -> Result<Option<RgbaFrame>, FrameSourceError> {
        Ok(self.frames.next())
    }

    fn frame_count_hint(&self) -> Option<u64> {
        self.total_frames
    }
}
