use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    AssetId, CURRENT_SCHEMA_VERSION, DomainError, DurationUs, FrameId, OverlayId, PhysicalPoint,
    PhysicalRect, PhysicalSize, ProjectId, ProjectRevision, TimeUs, TrackId, TransitionId,
    UnixTimeMs, ValidationIssue,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    Srgb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "color", rename_all = "snake_case")]
pub enum CanvasBackground {
    Transparent,
    Solid(Rgba),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub size: PhysicalSize,
    pub color_space: ColorSpace,
    pub background: CanvasBackground,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RasterEncoding {
    Rgba8,
    Qoi,
    Png,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetKind {
    Frame {
        size: PhysicalSize,
        encoding: RasterEncoding,
    },
    OverlayImage {
        size: PhysicalSize,
        encoding: RasterEncoding,
    },
    Mask {
        size: PhysicalSize,
        encoding: RasterEncoding,
    },
    ImportedSource {
        media_type: String,
    },
}

impl AssetKind {
    pub const fn raster_size(&self) -> Option<PhysicalSize> {
        match self {
            Self::Frame { size, .. }
            | Self::OverlayImage { size, .. }
            | Self::Mask { size, .. } => Some(*size),
            Self::ImportedSource { .. } => None,
        }
    }

    pub const fn is_frame(&self) -> bool {
        matches!(self, Self::Frame { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetDescriptor {
    pub id: AssetId,
    pub byte_len: u64,
    pub kind: AssetKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClipTransform {
    pub crop: Option<PhysicalRect>,
    pub output_size: Option<PhysicalSize>,
    pub rotation: QuarterTurn,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarterTurn {
    #[default]
    Zero,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
    Other(u16),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyStroke {
    pub physical_key: String,
    pub display_text: Option<String>,
    pub pressed: bool,
    pub at: TimeUs,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub cursor_position: Option<PhysicalPoint>,
    pub cursor_asset: Option<AssetId>,
    pub pressed_mouse_buttons: Vec<MouseButton>,
    pub key_strokes: Vec<KeyStroke>,
    pub dropped_frames_before: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    Blur {
        region: PhysicalRect,
        radius: u16,
    },
    Pixelate {
        region: PhysicalRect,
        block_size: u16,
    },
    Darken {
        region: PhysicalRect,
        amount_percent: u8,
    },
    Lighten {
        region: PhysicalRect,
        amount_percent: u8,
    },
    Border {
        widths: EdgeWidths,
        color: Rgba,
    },
    Shadow {
        offset_x: i32,
        offset_y: i32,
        blur_radius: u16,
        color: Rgba,
    },
    Cinemagraph {
        mask_asset: AssetId,
        invert_mask: bool,
    },
}

impl Effect {
    pub const fn region(&self) -> Option<PhysicalRect> {
        match self {
            Self::Blur { region, .. }
            | Self::Pixelate { region, .. }
            | Self::Darken { region, .. }
            | Self::Lighten { region, .. } => Some(*region),
            _ => None,
        }
    }

    pub const fn referenced_asset(&self) -> Option<AssetId> {
        match self {
            Self::Cinemagraph { mask_asset, .. } => Some(*mask_asset),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeWidths {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameClip {
    pub id: FrameId,
    pub asset_id: AssetId,
    pub duration: DurationUs,
    pub transform: ClipTransform,
    pub capture_metadata: CaptureMetadata,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimelineSpan {
    pub start: TimeUs,
    pub duration: DurationUs,
}

impl TimelineSpan {
    pub fn end(self) -> Option<TimeUs> {
        self.start.checked_add_duration(self.duration)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalAlignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Line,
    Arrow,
    Rectangle,
    Ellipse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub point: PhysicalPoint,
    pub pressure_milli: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayContent {
    Raster {
        asset_id: AssetId,
        position: PhysicalPoint,
        size: PhysicalSize,
        opacity: u8,
    },
    Text {
        text: String,
        position: PhysicalPoint,
        max_width: Option<crate::PhysicalPx>,
        font_family: String,
        font_size_px: u16,
        foreground: Rgba,
        background: Option<Rgba>,
        alignment: HorizontalAlignment,
    },
    Shape {
        kind: ShapeKind,
        bounds: PhysicalRect,
        stroke_width: u16,
        stroke: Rgba,
        fill: Option<Rgba>,
    },
    Drawing {
        points: Vec<StrokePoint>,
        width: u16,
        color: Rgba,
    },
    KeyStroke {
        text: String,
        position: PhysicalPoint,
    },
    Cursor {
        cursor_asset: Option<AssetId>,
        position: PhysicalPoint,
    },
    MouseClick {
        position: PhysicalPoint,
        button: MouseButton,
        color: Rgba,
        radius: u16,
    },
    Progress {
        bounds: PhysicalRect,
        foreground: Rgba,
        background: Rgba,
        show_frame_number: bool,
    },
}

impl OverlayContent {
    pub const fn referenced_asset(&self) -> Option<AssetId> {
        match self {
            Self::Raster { asset_id, .. } => Some(*asset_id),
            Self::Cursor {
                cursor_asset: Some(asset_id),
                ..
            } => Some(*asset_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverlayItem {
    pub id: OverlayId,
    pub span: TimelineSpan,
    pub z_index: i32,
    pub content: OverlayContent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverlayTrack {
    pub id: TrackId,
    pub name: String,
    pub visible: bool,
    pub opacity: u8,
    pub blend_mode: BlendMode,
    pub items: Vec<OverlayItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlideDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionKind {
    FadeToNext,
    FadeToColor { color: Rgba },
    Slide { direction: SlideDirection },
}

/// Step count assigned when deserializing manifests written before transition frames were exported.
pub const DEFAULT_TRANSITION_STEPS: u16 = 1;
/// Maximum number of intermediate frames generated by one transition.
pub const MAX_TRANSITION_STEPS: u16 = 1_024;

const fn default_transition_steps() -> u16 {
    DEFAULT_TRANSITION_STEPS
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    pub from_frame: FrameId,
    pub to_frame: FrameId,
    /// Total added duration distributed across generated intermediate frames.
    pub duration: DurationUs,
    /// Number of intermediate frames, excluding both original endpoints.
    #[serde(default = "default_transition_steps")]
    pub steps: u16,
    pub kind: TransitionKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub frames: Vec<FrameClip>,
    pub overlay_tracks: Vec<OverlayTrack>,
    pub transitions: Vec<Transition>,
}

impl Timeline {
    pub fn total_duration(&self) -> Option<TimeUs> {
        self.frames
            .iter()
            .try_fold(0_u64, |total, frame| {
                total.checked_add(frame.duration.get())
            })
            .map(TimeUs::new)
    }

    pub fn frame_start(&self, frame_id: FrameId) -> Option<TimeUs> {
        let mut start = 0_u64;
        for frame in &self.frames {
            if frame.id == frame_id {
                return Some(TimeUs::new(start));
            }
            start = start.checked_add(frame.duration.get())?;
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GifLoop {
    Infinite,
    Finite(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GifPaletteStrategy {
    Global,
    PerFrame,
    Adaptive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GifExportPreset {
    pub colors: u16,
    pub palette: GifPaletteStrategy,
    pub repeat: GifLoop,
    pub alpha_threshold: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceProvenance {
    Screen {
        source_label: Option<String>,
    },
    Camera {
        device_label: Option<String>,
    },
    Board,
    Imported {
        display_name: String,
        media_type: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub schema_version: u32,
    pub project_id: ProjectId,
    pub revision: ProjectRevision,
    pub app_version: String,
    pub created_at: UnixTimeMs,
    pub canvas: Canvas,
    pub timeline: Timeline,
    pub assets: BTreeMap<AssetId, AssetDescriptor>,
    pub export_presets: BTreeMap<String, GifExportPreset>,
    pub source_provenance: Vec<SourceProvenance>,
}

impl ProjectManifest {
    pub fn new(
        project_id: ProjectId,
        app_version: impl Into<String>,
        created_at: UnixTimeMs,
        canvas: Canvas,
    ) -> Result<Self, DomainError> {
        let manifest = Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_id,
            revision: ProjectRevision::ZERO,
            app_version: app_version.into(),
            created_at,
            canvas,
            timeline: Timeline::default(),
            assets: BTreeMap::new(),
            export_presets: BTreeMap::new(),
            source_provenance: Vec::new(),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let mut issues = Vec::new();
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            issues.push(ValidationIssue::UnsupportedSchema {
                found: self.schema_version,
                current: CURRENT_SCHEMA_VERSION,
            });
        }
        if self.project_id.is_nil() {
            issues.push(ValidationIssue::NilProjectId);
        }
        if self.app_version.trim().is_empty() {
            issues.push(ValidationIssue::EmptyAppVersion);
        }
        if self.canvas.size.validate().is_err() {
            issues.push(ValidationIssue::EmptyCanvas);
        }

        for (key, asset) in &self.assets {
            if *key != asset.id {
                issues.push(ValidationIssue::AssetKeyMismatch {
                    key: *key,
                    descriptor: asset.id,
                });
            }
            if asset.byte_len == 0 {
                issues.push(ValidationIssue::EmptyAsset { asset_id: asset.id });
            }
            if asset
                .kind
                .raster_size()
                .is_some_and(|size| size.validate().is_err())
            {
                issues.push(ValidationIssue::InvalidAssetSize { asset_id: asset.id });
            }
        }

        let mut frame_ids = BTreeSet::new();
        for (index, frame) in self.timeline.frames.iter().enumerate() {
            if frame.id.is_nil() {
                issues.push(ValidationIssue::NilFrameId { index });
            }
            if !frame_ids.insert(frame.id) {
                issues.push(ValidationIssue::DuplicateFrameId { frame_id: frame.id });
            }
            match self.assets.get(&frame.asset_id) {
                None => issues.push(ValidationIssue::MissingFrameAsset {
                    frame_id: frame.id,
                    asset_id: frame.asset_id,
                }),
                Some(asset) if !asset.kind.is_frame() => {
                    issues.push(ValidationIssue::IncompatibleFrameAsset {
                        frame_id: frame.id,
                        asset_id: frame.asset_id,
                    });
                }
                Some(asset) => {
                    if let (Some(crop), Some(size)) =
                        (frame.transform.crop, asset.kind.raster_size())
                        && !crop.fits_within(size)
                    {
                        issues.push(ValidationIssue::CropOutsideAsset { frame_id: frame.id });
                    }
                }
            }
            if frame
                .transform
                .output_size
                .is_some_and(|size| size.validate().is_err())
            {
                issues.push(ValidationIssue::InvalidAssetSize {
                    asset_id: frame.asset_id,
                });
            }
            for effect in &frame.effects {
                if effect
                    .region()
                    .is_some_and(|region| !region.fits_within(self.canvas.size))
                {
                    issues.push(ValidationIssue::EffectRegionOutsideCanvas { frame_id: frame.id });
                }
                if let Some(asset_id) = effect.referenced_asset()
                    && !self.assets.contains_key(&asset_id)
                {
                    issues.push(ValidationIssue::MissingEffectAsset {
                        frame_id: frame.id,
                        asset_id,
                    });
                }
            }
        }

        let timeline_duration = self.timeline.total_duration();
        if timeline_duration.is_none() {
            issues.push(ValidationIssue::TimelineDurationOverflow);
        }
        let timeline_duration = timeline_duration.unwrap_or(TimeUs::ZERO);

        let mut track_ids = BTreeSet::new();
        let mut overlay_ids = BTreeSet::new();
        for track in &self.timeline.overlay_tracks {
            if track.id.is_nil() {
                issues.push(ValidationIssue::NilTrackId);
            }
            if !track_ids.insert(track.id) {
                issues.push(ValidationIssue::DuplicateTrackId { track_id: track.id });
            }
            if track.name.trim().is_empty() {
                issues.push(ValidationIssue::EmptyTrackName { track_id: track.id });
            }
            for overlay in &track.items {
                if overlay.id.is_nil() {
                    issues.push(ValidationIssue::NilOverlayId);
                }
                if !overlay_ids.insert(overlay.id) {
                    issues.push(ValidationIssue::DuplicateOverlayId {
                        overlay_id: overlay.id,
                    });
                }
                if overlay.span.end().is_none_or(|end| end > timeline_duration) {
                    issues.push(ValidationIssue::OverlayOutsideTimeline {
                        overlay_id: overlay.id,
                    });
                }
                if let Some(asset_id) = overlay.content.referenced_asset()
                    && !self.assets.contains_key(&asset_id)
                {
                    issues.push(ValidationIssue::MissingOverlayAsset {
                        overlay_id: overlay.id,
                        asset_id,
                    });
                }
            }
        }

        let frame_positions: BTreeMap<_, _> = self
            .timeline
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| (frame.id, index))
            .collect();
        let mut transition_ids = BTreeSet::new();
        let mut transition_endpoints = BTreeMap::new();
        for transition in &self.timeline.transitions {
            if transition.id.is_nil() {
                issues.push(ValidationIssue::NilTransitionId);
            }
            if !transition_ids.insert(transition.id) {
                issues.push(ValidationIssue::DuplicateTransitionId {
                    transition_id: transition.id,
                });
            }
            if let Some(first_transition_id) = transition_endpoints
                .insert((transition.from_frame, transition.to_frame), transition.id)
            {
                issues.push(ValidationIssue::DuplicateTransitionEndpoints {
                    first_transition_id,
                    duplicate_transition_id: transition.id,
                    from_frame: transition.from_frame,
                    to_frame: transition.to_frame,
                });
            }
            if transition.steps == 0 || transition.steps > MAX_TRANSITION_STEPS {
                issues.push(ValidationIssue::InvalidTransitionSteps {
                    transition_id: transition.id,
                    steps: transition.steps,
                    maximum: MAX_TRANSITION_STEPS,
                });
            }
            if transition.duration.get() < u64::from(transition.steps) {
                issues.push(ValidationIssue::TransitionDurationTooShort {
                    transition_id: transition.id,
                    duration_us: transition.duration.get(),
                    steps: transition.steps,
                });
            }
            let Some(from_index) = frame_positions.get(&transition.from_frame) else {
                issues.push(ValidationIssue::TransitionFrameMissing {
                    transition_id: transition.id,
                    frame_id: transition.from_frame,
                });
                continue;
            };
            let Some(to_index) = frame_positions.get(&transition.to_frame) else {
                issues.push(ValidationIssue::TransitionFrameMissing {
                    transition_id: transition.id,
                    frame_id: transition.to_frame,
                });
                continue;
            };
            if from_index.checked_add(1) != Some(*to_index) {
                issues.push(ValidationIssue::TransitionFramesNotAdjacent {
                    transition_id: transition.id,
                });
            }
        }

        for (name, preset) in &self.export_presets {
            if name.trim().is_empty() {
                issues.push(ValidationIssue::InvalidExportPresetName);
            }
            if !(2..=256).contains(&preset.colors) {
                issues.push(ValidationIssue::InvalidExportColorCount {
                    preset: name.clone(),
                    colors: preset.colors,
                });
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(DomainError::InvalidManifest(issues))
        }
    }

    pub fn references_asset(&self, asset_id: AssetId) -> bool {
        self.timeline.frames.iter().any(|frame| {
            frame.asset_id == asset_id
                || frame.capture_metadata.cursor_asset == Some(asset_id)
                || frame
                    .effects
                    .iter()
                    .any(|effect| effect.referenced_asset() == Some(asset_id))
        }) || self.timeline.overlay_tracks.iter().any(|track| {
            track
                .items
                .iter()
                .any(|item| item.content.referenced_asset() == Some(asset_id))
        })
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    pub fn manifest() -> ProjectManifest {
        ProjectManifest::new(
            ProjectId::from_u128(1),
            "0.1.0",
            UnixTimeMs::new(1_700_000_000_000),
            Canvas {
                size: PhysicalSize::new(320, 200).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap()
    }

    pub fn asset(number: u8) -> AssetDescriptor {
        AssetDescriptor {
            id: AssetId::from_digest([number; 32]),
            byte_len: 320 * 200 * 4,
            kind: AssetKind::Frame {
                size: PhysicalSize::new(320, 200).unwrap(),
                encoding: RasterEncoding::Rgba8,
            },
        }
    }

    pub fn frame(number: u8, asset_id: AssetId) -> FrameClip {
        FrameClip {
            id: FrameId::from_u128(u128::from(number)),
            asset_id,
            duration: DurationUs::new(100_000).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_fixtures::*, *};

    #[test]
    fn detects_duplicate_frame_identity_and_missing_assets() {
        let mut manifest = manifest();
        let missing = AssetId::from_digest([9; 32]);
        manifest.timeline.frames = vec![frame(1, missing), frame(1, missing)];
        let DomainError::InvalidManifest(issues) = manifest.validate().unwrap_err() else {
            panic!("wrong error");
        };
        assert!(issues.contains(&ValidationIssue::DuplicateFrameId {
            frame_id: FrameId::from_u128(1)
        }));
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, ValidationIssue::MissingFrameAsset { .. }))
        );
    }

    #[test]
    fn timeline_duration_is_derived_not_persisted() {
        let mut timeline = Timeline::default();
        let asset_id = AssetId::from_digest([1; 32]);
        timeline.frames.push(frame(1, asset_id));
        timeline.frames.push(frame(2, asset_id));
        assert_eq!(timeline.total_duration(), Some(TimeUs::new(200_000)));
        assert_eq!(
            timeline.frame_start(FrameId::from_u128(2)),
            Some(TimeUs::new(100_000))
        );
    }

    #[test]
    fn btree_asset_map_serializes_with_hex_keys() {
        let mut manifest = manifest();
        let asset = asset(7);
        manifest.assets.insert(asset.id, asset);
        let json = serde_json::to_string(&manifest).unwrap();
        let decoded: ProjectManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn transition_steps_validate_without_limiting_added_duration() {
        let mut manifest = manifest();
        let asset = asset(1);
        manifest.assets.insert(asset.id, asset.clone());
        manifest.timeline.frames = vec![frame(1, asset.id), frame(2, asset.id)];
        manifest.timeline.transitions = vec![Transition {
            id: TransitionId::from_u128(1),
            from_frame: FrameId::from_u128(1),
            to_frame: FrameId::from_u128(2),
            duration: DurationUs::new(500_000).unwrap(),
            steps: MAX_TRANSITION_STEPS,
            kind: TransitionKind::FadeToNext,
        }];
        assert!(manifest.validate().is_ok());

        let mut duplicate = manifest.timeline.transitions[0].clone();
        duplicate.id = TransitionId::from_u128(2);
        manifest.timeline.transitions.push(duplicate);
        let DomainError::InvalidManifest(issues) = manifest.validate().unwrap_err() else {
            panic!("wrong error");
        };
        assert!(
            issues.contains(&ValidationIssue::DuplicateTransitionEndpoints {
                first_transition_id: TransitionId::from_u128(1),
                duplicate_transition_id: TransitionId::from_u128(2),
                from_frame: FrameId::from_u128(1),
                to_frame: FrameId::from_u128(2),
            })
        );
        manifest.timeline.transitions.pop();

        for steps in [0, MAX_TRANSITION_STEPS + 1] {
            manifest.timeline.transitions[0].steps = steps;
            let DomainError::InvalidManifest(issues) = manifest.validate().unwrap_err() else {
                panic!("wrong error");
            };
            assert!(issues.contains(&ValidationIssue::InvalidTransitionSteps {
                transition_id: TransitionId::from_u128(1),
                steps,
                maximum: MAX_TRANSITION_STEPS,
            }));
        }
    }

    #[test]
    fn legacy_transition_json_defaults_to_one_step_without_schema_change() {
        let transition = Transition {
            id: TransitionId::from_u128(1),
            from_frame: FrameId::from_u128(2),
            to_frame: FrameId::from_u128(3),
            duration: DurationUs::new(10_000).unwrap(),
            steps: 7,
            kind: TransitionKind::FadeToNext,
        };
        let mut json = serde_json::to_value(&transition).unwrap();
        json.as_object_mut().unwrap().remove("steps");

        let decoded: Transition = serde_json::from_value(json).unwrap();

        assert_eq!(decoded.steps, DEFAULT_TRANSITION_STEPS);
        assert_eq!(CURRENT_SCHEMA_VERSION, 1);
    }
}
