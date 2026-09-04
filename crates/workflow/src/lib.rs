//! Synchronous capture-to-GIF application workflows.
//!
//! The functions in this crate do not create threads or depend on an async
//! runtime. All participating ports are `Send`, so an application can move a
//! complete call to its own background worker while retaining control over job
//! scheduling and UI dispatch.

#![forbid(unsafe_code)]

mod collect;
mod control;
mod error;
mod output;
mod progress;

pub use collect::{
    CollectOptions, CollectedRecording, CollectionLimit, CollectionSummary, collect,
    collect_controlled,
};
pub use control::{RecordingControl, RecordingController, TargetUpdateRequest, TargetUpdateStatus};
pub use error::WorkflowError;
pub use output::{
    RecordToGifOptions, RecordToGifReport, partial_output_path, record_to_gif,
    record_to_gif_controlled, record_to_gif_with_encoder,
};
pub use progress::{NoopWorkflowProgress, WorkflowPhase, WorkflowProgress, WorkflowProgressSink};
