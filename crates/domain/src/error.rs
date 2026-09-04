use std::{error::Error, fmt};

use crate::{AssetId, FrameId, OverlayId, TrackId, TransitionId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdParseError {
    WrongLength { expected: usize, actual: usize },
    InvalidHex { index: usize },
}

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { expected, actual } => {
                write!(
                    formatter,
                    "identifier must contain {expected} hex characters, got {actual}"
                )
            }
            Self::InvalidHex { index } => {
                write!(
                    formatter,
                    "identifier contains non-hex data at byte {index}"
                )
            }
        }
    }
}

impl Error for IdParseError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitError {
    ZeroDuration,
    EmptyPhysicalSize,
    PhysicalAreaOverflow,
    PhysicalCoordinateOverflow,
    NonFiniteLogicalPoint,
    InvalidScaleFactor,
}

impl fmt::Display for UnitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroDuration => "duration must be greater than zero",
            Self::EmptyPhysicalSize => "physical width and height must be greater than zero",
            Self::PhysicalAreaOverflow => "physical pixel area overflows u64",
            Self::PhysicalCoordinateOverflow => "physical rectangle coordinate overflows u32",
            Self::NonFiniteLogicalPoint => "logical point must be finite",
            Self::InvalidScaleFactor => "scale factor must be finite and greater than zero",
        })
    }
}

impl Error for UnitError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationIssue {
    UnsupportedSchema {
        found: u32,
        current: u32,
    },
    NilProjectId,
    EmptyAppVersion,
    EmptyCanvas,
    AssetKeyMismatch {
        key: AssetId,
        descriptor: AssetId,
    },
    EmptyAsset {
        asset_id: AssetId,
    },
    InvalidAssetSize {
        asset_id: AssetId,
    },
    NilFrameId {
        index: usize,
    },
    DuplicateFrameId {
        frame_id: FrameId,
    },
    MissingFrameAsset {
        frame_id: FrameId,
        asset_id: AssetId,
    },
    IncompatibleFrameAsset {
        frame_id: FrameId,
        asset_id: AssetId,
    },
    CropOutsideAsset {
        frame_id: FrameId,
    },
    TimelineDurationOverflow,
    NilTrackId,
    DuplicateTrackId {
        track_id: TrackId,
    },
    EmptyTrackName {
        track_id: TrackId,
    },
    NilOverlayId,
    DuplicateOverlayId {
        overlay_id: OverlayId,
    },
    OverlayOutsideTimeline {
        overlay_id: OverlayId,
    },
    MissingOverlayAsset {
        overlay_id: OverlayId,
        asset_id: AssetId,
    },
    EffectRegionOutsideCanvas {
        frame_id: FrameId,
    },
    MissingEffectAsset {
        frame_id: FrameId,
        asset_id: AssetId,
    },
    NilTransitionId,
    DuplicateTransitionId {
        transition_id: TransitionId,
    },
    DuplicateTransitionEndpoints {
        first_transition_id: TransitionId,
        duplicate_transition_id: TransitionId,
        from_frame: FrameId,
        to_frame: FrameId,
    },
    TransitionFrameMissing {
        transition_id: TransitionId,
        frame_id: FrameId,
    },
    TransitionFramesNotAdjacent {
        transition_id: TransitionId,
    },
    InvalidTransitionSteps {
        transition_id: TransitionId,
        steps: u16,
        maximum: u16,
    },
    TransitionDurationTooShort {
        transition_id: TransitionId,
        duration_us: u64,
        steps: u16,
    },
    TransitionTooLong {
        transition_id: TransitionId,
    },
    InvalidExportPresetName,
    InvalidExportColorCount {
        preset: String,
        colors: u16,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidManifest(Vec<ValidationIssue>),
    RevisionOverflow,
    FrameIndexOutOfBounds { index: usize, len: usize },
    UnknownFrame(FrameId),
    UnknownAsset(AssetId),
    UnknownTrack(TrackId),
    DuplicateTrackId(TrackId),
    TrackIndexOutOfBounds { index: usize, len: usize },
    DuplicateFrameInCommand(FrameId),
    DuplicateFrameId(FrameId),
    DuplicateAssetId(AssetId),
    FrameIdentityMismatch { expected: FrameId, actual: FrameId },
    ReorderDoesNotMatchTimeline,
    RestoreIndexOutOfBounds { index: usize, len: usize },
    EmptyCommand,
    AssetStillReferenced(AssetId),
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(issues) => {
                write!(
                    formatter,
                    "project manifest violates {} invariant(s)",
                    issues.len()
                )
            }
            Self::RevisionOverflow => formatter.write_str("project revision overflow"),
            Self::FrameIndexOutOfBounds { index, len } => {
                write!(
                    formatter,
                    "frame insertion index {index} exceeds timeline length {len}"
                )
            }
            Self::UnknownFrame(id) => write!(formatter, "unknown frame {id}"),
            Self::UnknownAsset(id) => write!(formatter, "unknown asset {id}"),
            Self::UnknownTrack(id) => write!(formatter, "unknown overlay track {id}"),
            Self::DuplicateTrackId(id) => write!(formatter, "overlay track {id} already exists"),
            Self::TrackIndexOutOfBounds { index, len } => {
                write!(
                    formatter,
                    "overlay track index {index} exceeds track count {len}"
                )
            }
            Self::DuplicateFrameInCommand(id) => {
                write!(formatter, "frame {id} occurs more than once in command")
            }
            Self::DuplicateFrameId(id) => write!(formatter, "frame {id} already exists"),
            Self::DuplicateAssetId(id) => write!(formatter, "asset {id} already exists"),
            Self::FrameIdentityMismatch { expected, actual } => {
                write!(
                    formatter,
                    "replacement frame id {actual} does not match {expected}"
                )
            }
            Self::ReorderDoesNotMatchTimeline => {
                formatter.write_str("reorder must contain every timeline frame exactly once")
            }
            Self::RestoreIndexOutOfBounds { index, len } => {
                write!(
                    formatter,
                    "restore index {index} exceeds timeline length {len}"
                )
            }
            Self::EmptyCommand => formatter.write_str("compound edit command must not be empty"),
            Self::AssetStillReferenced(id) => write!(formatter, "asset {id} is still referenced"),
        }
    }
}

impl Error for DomainError {}
