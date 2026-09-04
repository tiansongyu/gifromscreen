//! Pure domain model for GifFromScreen.
//!
//! This crate deliberately contains no filesystem, operating-system, UI, codec,
//! clock, or random-number access. Identifiers and wall-clock timestamps are
//! supplied by the application boundary.

#![forbid(unsafe_code)]

mod command;
mod error;
mod id;
mod model;
mod units;

pub use command::{AppliedEdit, EditCommand, FrameDurationChange, IndexedFrame};
pub use error::{DomainError, IdParseError, UnitError, ValidationIssue};
pub use id::{AssetId, FrameId, OverlayId, ProjectId, TrackId, TransitionId};
pub use model::*;
pub use units::*;

/// Current on-disk manifest schema understood by this version of the domain.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
