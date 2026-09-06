//! Serializable authoring options shared by manual annotations and automatic edit tasks.

use serde::{Deserialize, Serialize};

use crate::{MouseButton, PhysicalPoint, PhysicalSize, ProgressDirection, Rgba};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressMeasure {
    #[default]
    Frames,
    ElapsedTime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressOptions {
    pub direction: ProgressDirection,
    pub measure: ProgressMeasure,
    pub remaining: bool,
    pub show_bar: bool,
    pub bar_color: Rgba,
    /// Supported tokens: {frame}, {frames}, {elapsed}, {total}, {remaining}, {percent}.
    pub format: String,
}

impl Default for ProgressOptions {
    fn default() -> Self {
        Self {
            direction: ProgressDirection::LeftToRight,
            measure: ProgressMeasure::Frames,
            remaining: false,
            show_bar: true,
            bar_color: Rgba {
                red: 0,
                green: 102,
                blue: 220,
                alpha: 255,
            },
            format: "{frame} / {frames}".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "options", rename_all = "snake_case")]
pub enum AnnotationMode {
    Progress(ProgressOptions),
    ManualKeys { text: String },
    RecordedKeys,
    ManualClick { button: MouseButton },
    RecordedClicks,
    RecordedCursor,
    BuiltinCursor,
}

impl AnnotationMode {
    /// Whether this mode actually shapes text (a bar-only progress overlay does not).
    pub fn uses_text(&self) -> bool {
        match self {
            Self::Progress(options) => !options.format.trim().is_empty(),
            Self::ManualKeys { .. } | Self::RecordedKeys => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationRequest {
    pub mode: AnnotationMode,
    pub position: PhysicalPoint,
    /// Text box and/or progress bar dimensions in physical canvas pixels.
    pub size: PhysicalSize,
    pub font_family: String,
    pub font_size_px: u16,
    pub foreground: Rgba,
    pub background: Rgba,
    pub opacity: u8,
    pub z_index: i32,
    pub click_radius: u16,
    /// Recorded event labels remain visible for this many milliseconds.
    pub hold_ms: u32,
}

impl Default for AnnotationRequest {
    fn default() -> Self {
        Self {
            mode: AnnotationMode::Progress(ProgressOptions::default()),
            position: PhysicalPoint::default(),
            size: PhysicalSize::new(240, 40).expect("nonempty default annotation"),
            font_family: "sans-serif".to_owned(),
            font_size_px: 20,
            foreground: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            background: Rgba {
                red: 24,
                green: 28,
                blue: 40,
                alpha: 180,
            },
            opacity: 255,
            z_index: 10,
            click_radius: 14,
            hold_ms: 500,
        }
    }
}

impl AnnotationRequest {
    /// Validate a saved preset before a target canvas is available.
    pub fn validate_settings(&self) -> Result<(), String> {
        self.validate(PhysicalSize::new(u32::MAX, u32::MAX).expect("nonzero bounds"))
    }

    /// Check all resource and placement limits before loading fonts or writing assets.
    pub fn validate(&self, canvas: PhysicalSize) -> Result<(), String> {
        let uses_text = self.mode.uses_text();
        let uses_clicks = matches!(
            self.mode,
            AnnotationMode::ManualClick { .. } | AnnotationMode::RecordedClicks
        );
        if self.opacity == 0 || (uses_text || uses_clicks) && self.foreground.alpha == 0 {
            return Err("Choose a visible annotation color and opacity.".to_owned());
        }
        let uses_box = matches!(
            self.mode,
            AnnotationMode::Progress(_)
                | AnnotationMode::ManualKeys { .. }
                | AnnotationMode::RecordedKeys
        );
        let manual_position = uses_box
            || matches!(
                self.mode,
                AnnotationMode::ManualClick { .. } | AnnotationMode::BuiltinCursor
            );
        if manual_position && (self.position.x >= canvas.width || self.position.y >= canvas.height)
        {
            return Err("Place the annotation inside the canvas.".to_owned());
        }
        if uses_box
            && (self.size.validate().is_err()
                || self.size.width.get() > 4096
                || self.size.height.get() > 4096
                || (u64::from(self.position.x.get()) + u64::from(self.size.width.get())
                    > u64::from(canvas.width.get())
                    || u64::from(self.position.y.get()) + u64::from(self.size.height.get())
                        > u64::from(canvas.height.get())))
        {
            return Err(
                "The annotation box must fit inside the canvas, up to 4096 pixels per edge."
                    .to_owned(),
            );
        }
        if uses_text
            && (self.font_family.trim().is_empty()
                || self.font_family.len() > 256
                || !(1..=512).contains(&self.font_size_px))
        {
            return Err(
                "Choose a font family of 1–256 bytes and a font size of 1–512 pixels.".to_owned(),
            );
        }
        if uses_clicks && !(1..=1024).contains(&self.click_radius) {
            return Err("Click radius must be between 1 and 1024 pixels.".to_owned());
        }
        if matches!(
            self.mode,
            AnnotationMode::RecordedKeys | AnnotationMode::RecordedClicks
        ) && !(1..=60_000).contains(&self.hold_ms)
        {
            return Err(
                "Recorded event hold time must be between 1 and 60000 milliseconds.".to_owned(),
            );
        }
        match &self.mode {
            AnnotationMode::ManualKeys { text } if text.trim().is_empty() || text.len() > 4096 => {
                return Err("Enter between 1 and 4096 UTF-8 bytes for the key label.".to_owned());
            }
            AnnotationMode::Progress(options) => {
                if options.format.len() > 1024
                    || (!options.show_bar && options.format.trim().is_empty())
                {
                    return Err("Use a progress bar or a label of at most 1024 bytes.".to_owned());
                }
                let mut remainder = options.format.as_str();
                while let Some((_, token)) = remainder.split_once('{') {
                    let Some((name, rest)) = token.split_once('}') else {
                        return Err("Close each progress label token with }.".to_owned());
                    };
                    if ![
                        "frame",
                        "frames",
                        "elapsed",
                        "total",
                        "remaining",
                        "percent",
                    ]
                    .contains(&name)
                    {
                        return Err(format!("Unknown progress label token: {{{name}}}."));
                    }
                    remainder = rest;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OverlayContent, OverlayTrack};

    #[test]
    fn cursor_validation_ignores_unused_style_but_requires_real_opacity_and_manual_position() {
        let canvas = PhysicalSize::new(8, 8).unwrap();
        for mode in [
            AnnotationMode::RecordedCursor,
            AnnotationMode::BuiltinCursor,
        ] {
            let mut request = AnnotationRequest {
                mode,
                font_family: String::new(),
                font_size_px: 0,
                size: PhysicalSize {
                    width: crate::PhysicalPx::ZERO,
                    height: crate::PhysicalPx::ZERO,
                },
                foreground: Rgba::TRANSPARENT,
                click_radius: 0,
                hold_ms: 0,
                ..AnnotationRequest::default()
            };
            assert!(request.validate_settings().is_ok());
            assert!(request.validate(canvas).is_ok());
            request.opacity = 0;
            assert!(request.validate(canvas).is_err());
            request.opacity = 255;
            request.position.x = crate::PhysicalPx::new(8);
            assert_eq!(
                request.validate(canvas).is_ok(),
                matches!(request.mode, AnnotationMode::RecordedCursor)
            );
        }
    }

    #[test]
    fn text_click_and_hold_fields_are_checked_only_when_consumed() {
        let mut request = AnnotationRequest {
            mode: AnnotationMode::ManualKeys {
                text: "C".to_owned(),
            },
            font_size_px: 0,
            click_radius: 0,
            hold_ms: 0,
            ..AnnotationRequest::default()
        };
        assert!(request.validate_settings().is_err());
        request.font_size_px = 20;
        assert!(request.validate_settings().is_ok());
        request.mode = AnnotationMode::ManualClick {
            button: MouseButton::Left,
        };
        request.font_family.clear();
        assert!(request.validate_settings().is_err());
        request.click_radius = 1;
        assert!(request.validate_settings().is_ok());
        request.mode = AnnotationMode::RecordedClicks;
        assert!(request.validate_settings().is_err());
        request.hold_ms = 1;
        assert!(request.validate_settings().is_ok());
        request.mode = AnnotationMode::Progress(ProgressOptions {
            format: String::new(),
            ..ProgressOptions::default()
        });
        request.foreground = Rgba::TRANSPARENT;
        assert!(request.validate_settings().is_ok());
    }

    #[test]
    fn legacy_overlay_json_keeps_its_original_meaning() {
        let keys: OverlayContent = serde_json::from_str(
            r#"{"type":"key_stroke","text":"Ctrl+C","position":{"x":0,"y":0}}"#,
        )
        .unwrap();
        assert!(matches!(
            keys,
            OverlayContent::KeyStroke { raster: None, .. }
        ));
        let cursor: OverlayContent = serde_json::from_str(
            r#"{"type":"cursor","cursor_asset":null,"position":{"x":1,"y":2}}"#,
        )
        .unwrap();
        assert!(
            matches!(cursor,OverlayContent::Cursor{hotspot:PhysicalPoint{x,y},..} if x.get()==0 && y.get()==0)
        );
        let track:OverlayTrack=serde_json::from_str(r#"{"id":"00000000000000000000000000000001","name":"legacy","visible":true,"opacity":255,"blend_mode":"normal","items":[]}"#).unwrap();
        assert!(track.annotation.is_none());
    }

    #[test]
    fn saved_options_round_trip_and_default_missing_new_fields() {
        let request = AnnotationRequest::default();
        let value = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<AnnotationRequest>(&value).unwrap(),
            request
        );
        assert_eq!(
            serde_json::from_str::<AnnotationRequest>("{}").unwrap(),
            request
        );
        assert_eq!(
            serde_json::from_str::<ProgressOptions>("{}").unwrap(),
            ProgressOptions::default()
        );
    }

    #[test]
    fn recorded_geometry_does_not_require_an_unused_text_box_to_fit() {
        let small = PhysicalSize::new(8, 8).unwrap();
        let mut request = AnnotationRequest {
            mode: AnnotationMode::RecordedClicks,
            ..AnnotationRequest::default()
        };
        assert!(request.validate(small).is_ok());
        request.mode = AnnotationMode::RecordedCursor;
        assert!(request.validate(small).is_ok());
        request.mode = AnnotationMode::Progress(ProgressOptions::default());
        assert!(request.validate(small).is_err());
    }
}
