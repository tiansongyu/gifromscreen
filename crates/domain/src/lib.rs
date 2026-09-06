//! Pure domain model for GifFromScreen.
//!
//! This crate deliberately contains no filesystem, operating-system, UI, codec,
//! clock, or random-number access. Identifiers and wall-clock timestamps are
//! supplied by the application boundary.

#![forbid(unsafe_code)]

mod annotation_options;
mod annotation_scope;
mod capture_binding;
mod command;
mod editing_task;
mod error;
mod export_preset;
mod id;
mod model;
mod overlay_timing;
mod progress_fraction;
mod task_run;
mod units;

pub use annotation_options::*;
pub use annotation_scope::*;
pub use capture_binding::{
    CaptureBinding, CaptureBindingSummary, CaptureReplayBlock, FrameCaptureBindingChange,
    capture_binding_summary, has_recorded_input, recorded_annotation_barrier,
    recorded_annotation_block,
};
pub use command::{AppliedEdit, EditCommand, FrameDurationChange, IndexedFrame};
pub use editing_task::*;
pub use error::{DomainError, IdParseError, UnitError, ValidationIssue};
pub use export_preset::*;
pub use id::{AssetId, FrameId, OverlayId, ProjectId, TrackId, TransitionId};
pub use model::*;
pub use progress_fraction::ProgressFraction;
pub use task_run::*;
pub use units::*;

/// Current on-disk manifest schema understood by this version of the domain.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
