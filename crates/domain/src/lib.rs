//! Pure domain model for GifFromScreen.
//!
//! This crate deliberately contains no filesystem, operating-system, UI, codec,
//! clock, or random-number access. Identifiers and wall-clock timestamps are
//! supplied by the application boundary.

#![forbid(unsafe_code)]

mod annotation_options;
mod annotation_scope;
mod capture_binding;
mod capture_clock;
mod command;
mod editing_task;
mod error;
mod export_preset;
mod frame_authoring;
mod frame_geometry;
#[cfg(test)]
mod frame_input_assets_tests;
mod frame_input_replay;
mod frame_overlay;
mod id;
mod image_effect;
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
    recorded_annotation_barrier_at_stage, recorded_annotation_block,
    recorded_annotation_block_at_stage,
};
pub use capture_clock::{CaptureClockContext, FrameCaptureClockChange};
pub use command::{AppliedEdit, EditCommand, FrameDurationChange, IndexedFrame};
pub use editing_task::*;
pub use error::{DomainError, IdParseError, UnitError, ValidationIssue};
pub use export_preset::*;
pub use frame_authoring::*;
pub use frame_geometry::*;
pub use frame_input_replay::*;
pub use frame_overlay::*;
pub use id::{AssetId, CaptureClockId, FrameId, OverlayId, ProjectId, TrackId, TransitionId};
pub use image_effect::*;
pub use model::*;
pub use progress_fraction::ProgressFraction;
pub use task_run::*;
pub use units::*;

/// Current on-disk manifest schema understood by this version of the domain.
pub const CURRENT_SCHEMA_VERSION: u32 = 6;
