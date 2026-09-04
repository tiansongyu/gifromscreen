use std::num::NonZeroU64;

/// Default maximum logical canvas width.
pub const DEFAULT_MAX_WIDTH: u16 = 8_192;
/// Default maximum logical canvas height.
pub const DEFAULT_MAX_HEIGHT: u16 = 8_192;
/// Default maximum number of decoded frames.
pub const DEFAULT_MAX_FRAMES: usize = 10_000;
/// Default maximum bytes retained across all returned RGBA frame buffers.
pub const DEFAULT_MAX_TOTAL_RGBA_BYTES: u64 = 512 * 1024 * 1024;
/// Default replacement duration for a GIF frame whose delay field is zero.
pub const DEFAULT_ZERO_DELAY_US: u64 = 10_000;

/// Resource limits applied before allocating pixels for untrusted GIF input.
///
/// `max_total_rgba_bytes` counts the full-canvas pixel buffers retained in the
/// returned animation. The decoder additionally uses a bounded working canvas
/// and at most a small number of frame-sized scratch buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    /// Maximum logical canvas width in pixels.
    pub max_width: u16,
    /// Maximum logical canvas height in pixels.
    pub max_height: u16,
    /// Maximum number of image frames.
    pub max_frames: usize,
    /// Maximum total bytes across all returned full-canvas RGBA8 frames.
    pub max_total_rgba_bytes: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_width: DEFAULT_MAX_WIDTH,
            max_height: DEFAULT_MAX_HEIGHT,
            max_frames: DEFAULT_MAX_FRAMES,
            max_total_rgba_bytes: DEFAULT_MAX_TOTAL_RGBA_BYTES,
        }
    }
}

/// Policy for GIF frames whose delay field is zero.
///
/// GIF stores delays in centiseconds. Although zero is legal in real-world
/// files, displaying it literally can create a busy animation loop, so the
/// default substitutes one centisecond.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZeroDelayPolicy {
    /// Replace a zero delay with the supplied positive duration in microseconds.
    UseMinimum(NonZeroU64),
    /// Reject the animation when a zero-delay frame is encountered.
    Reject,
}

impl Default for ZeroDelayPolicy {
    fn default() -> Self {
        let duration = NonZeroU64::new(DEFAULT_ZERO_DELAY_US)
            .expect("the default zero-delay duration is non-zero");
        Self::UseMinimum(duration)
    }
}

/// Configuration for [`crate::decode_gif`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GifDecodeOptions {
    /// Bounds for dimensions and retained output.
    pub limits: DecodeLimits,
    /// Handling for a frame delay of zero centiseconds.
    pub zero_delay: ZeroDelayPolicy,
}

/// Loop metadata carried by a GIF animation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoopBehavior {
    /// Play once; the input has no recognized loop extension.
    #[default]
    Once,
    /// Repeat indefinitely.
    Infinite,
    /// Preserve the positive finite count from the Netscape loop extension.
    Finite(u16),
}

/// One decoded, full-canvas straight-alpha RGBA8 frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedFrame {
    pub(crate) rgba: Vec<u8>,
    pub(crate) duration_us: u64,
}

impl DecodedFrame {
    /// Borrow the row-major RGBA8 pixel buffer.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Consume the frame and return its row-major RGBA8 pixel buffer.
    pub fn into_rgba(self) -> Vec<u8> {
        self.rgba
    }

    /// Frame display duration in microseconds.
    pub const fn duration_us(&self) -> u64 {
        self.duration_us
    }
}

/// A decoded GIF whose frames all have the logical canvas dimensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedAnimation {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) frames: Vec<DecodedFrame>,
    pub(crate) loop_behavior: LoopBehavior,
}

impl DecodedAnimation {
    /// Logical canvas width in physical pixels.
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Logical canvas height in physical pixels.
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Decoded full-canvas frames in playback order.
    pub fn frames(&self) -> &[DecodedFrame] {
        &self.frames
    }

    /// Consume the animation and return its frames.
    pub fn into_frames(self) -> Vec<DecodedFrame> {
        self.frames
    }

    /// Loop metadata decoded from the Netscape application extension.
    pub const fn loop_behavior(&self) -> LoopBehavior {
        self.loop_behavior
    }
}
