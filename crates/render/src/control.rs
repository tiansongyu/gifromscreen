/// Read-only cancellation boundary for a render operation.
///
/// The renderer checks this token between pipeline stages and while processing
/// rows. Implementations should make `is_cancelled` cheap and non-blocking.
pub trait CancellationToken: Send + Sync {
    /// Returns `true` when the current render should stop as soon as practical.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation token for callers that always render to completion.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancel;

impl CancellationToken for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}
