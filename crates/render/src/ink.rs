//! Bounded, subpixel ink geometry shared by authoring and CPU rasterization.
//! These are editing-time values; committed projects retain typed PM snapshots.

use crate::{PremultipliedSnapshotError, SurfaceError};
use thiserror::Error;

/// A point in physical image coordinates, retaining subpixel precision.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InkPoint {
    /// Horizontal position, which may lie outside the image before clipping.
    pub x: f64,
    /// Vertical position, which may lie outside the image before clipping.
    pub y: f64,
}

/// One stylus sample. Ordinary mouse input uses pressure 0.5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkSample {
    /// Physical subpixel position.
    pub position: InkPoint,
    /// Original normalized stylus pressure in 0..=1, not a diameter multiplier.
    pub pressure: f32,
}

/// The axis-aligned shape of the reference ink pen.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InkTip {
    /// Elliptical pen tip.
    #[default]
    Ellipse,
    /// Rectangular pen tip.
    Rectangle,
}

/// Attributes captured for an individual stroke, independent of later toolbar changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkAttributes {
    /// Physical tip width in 1..=100 pixels before pressure scaling.
    pub width: f64,
    /// Physical tip height in 1..=100 pixels before pressure scaling.
    pub height: f64,
    /// Ellipse or rectangle.
    pub tip: InkTip,
    /// Apply the reference Bezier fitting algorithm before contour expansion.
    pub fit_to_curve: bool,
    /// Force the normal tip size instead of using the sample pressure.
    pub ignore_pressure: bool,
}

impl Default for InkAttributes {
    fn default() -> Self {
        Self {
            width: 30.0,
            height: 30.0,
            tip: InkTip::Ellipse,
            fit_to_curve: false,
            ignore_pressure: false,
        }
    }
}

/// Raw, ordered stylus samples with their own immutable-on-authoring pen attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct InkStroke {
    /// At least one sample: a click is a legitimate single-point stroke.
    pub samples: Vec<InkSample>,
    /// Attributes for this stroke only.
    pub attributes: InkAttributes,
}

/// Filled-path winding rule, applied before union with other paths.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InkFillRule {
    /// Nonzero winding, used by WPF stroke outlines.
    #[default]
    NonZero,
    /// Odd crossings, used by explicitly even-odd paths.
    EvenOdd,
}

/// A contour segment whose start is the previous segment's endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InkSegment {
    /// Straight segment.
    LineTo(InkPoint),
    /// Cubic Bezier segment.
    CubicTo {
        /// First control point.
        control1: InkPoint,
        /// Second control point.
        control2: InkPoint,
        /// Endpoint.
        to: InkPoint,
    },
}

/// A contour, implicitly closed for filling even when authored as open.
#[derive(Clone, Debug, PartialEq)]
pub struct InkFigure {
    /// First point.
    pub start: InkPoint,
    /// Ordered line or cubic segments.
    pub segments: Vec<InkSegment>,
    /// Whether the geometric outline was explicitly closed.
    pub closed: bool,
}

/// A filled geometry. Multiple independent paths are combined by geometric union.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InkPath {
    /// Rule within this path; other paths cannot cancel its winding.
    pub fill_rule: InkFillRule,
    /// Contours, including any holes with appropriate winding.
    pub figures: Vec<InkFigure>,
}

/// Bounds on authoring-derived work, rather than trusting tiny final output dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InkLimits {
    /// Maximum source samples/control points.
    pub max_points: usize,
    /// Maximum generated or expanded segments.
    pub max_segments: usize,
    /// Maximum counted fitting, subdivision, scan and pixel operations.
    pub max_work: u64,
    /// Maximum owned image/geometry working bytes in an operation.
    pub max_bytes: usize,
}

impl Default for InkLimits {
    fn default() -> Self {
        Self {
            max_points: 32_768,
            max_segments: 262_144,
            max_work: 100_000_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Invalid geometry, bounded-work rejection, cancellation or invalid output pixels.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum InkError {
    /// Source parameters are malformed or cannot describe valid ink.
    #[error("invalid ink: {0}")]
    Invalid(String),
    /// An explicit geometry, work or memory bound was exceeded.
    #[error("ink limit exceeded: {0}")]
    Limit(String),
    /// No partially generated output is published after cancellation.
    #[error("ink operation cancelled")]
    Cancelled,
    /// Image shape or allocation failure.
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    /// The generated premultiplied image is invalid.
    #[error(transparent)]
    Snapshot(#[from] PremultipliedSnapshotError),
}
