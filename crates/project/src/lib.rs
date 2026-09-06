//! Durable active-project storage for GifFromScreen.
//!
//! The project format consists of an atomic manifest snapshot, an append-only
//! checksummed command journal, immutable content-addressed assets, disposable
//! caches, and an ownership lock. Platform capture and rendering do not belong
//! in this crate.

#![forbid(unsafe_code)]

mod active;
mod asset_store;
mod atomic_file;
mod error;
mod journal;
mod lock;
mod private_fs;

pub use active::{
    ActiveProject, AssetCheck, AssetIssue, CommitReceipt, OpenedProject, ProjectLayout,
};
pub use asset_store::AssetStore;
pub use error::ProjectError;
pub use journal::{JournalRecord, JournalRecoveryReport, JournalStopReason};
pub use lock::{LockInfo, LockPolicy};
