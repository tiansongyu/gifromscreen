//! Shape multilingual text once, then persist its RGBA pixels with the source attributes.
//!
//! Font discovery and rasterization belong on a worker thread. Export only reads the frozen asset,
//! so reopening a project never substitutes a different font. Bundled Ubuntu/Hack fonts provide
//! a baseline; complex scripts use installed fonts. Missing glyphs are reported, never hidden.

#![forbid(unsafe_code)]

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use gif_from_screen_domain::{HorizontalAlignment, PhysicalSize, Rgba};
use thiserror::Error;

/// Maximum UTF-8 source bytes accepted before invoking the shaper.
pub const MAX_TEXT_BYTES: usize = 16_384;
/// Maximum width and height of a rasterized caption.
pub const MAX_TEXT_EDGE: u32 = 4_096;

/// Original editable attributes of one bounded caption.
#[derive(Clone, Debug)]
pub struct TextRequest {
    /// Unicode source text, with explicit newlines allowed.
    pub text: String,
    /// Font family name, or `sans-serif`, `serif`, or `monospace`.
    pub font_family: String,
    /// Font size in physical pixels, from 1 through 512.
    pub font_size_px: u16,
    /// Exact wrapping and clipping box; at most 4096 pixels per edge.
    pub size: PhysicalSize,
    /// Foreground color, in straight-alpha sRGB.
    pub foreground: Rgba,
    /// Optional background for the entire text box.
    pub background: Option<Rgba>,
    /// Per-paragraph horizontal alignment within the text box.
    pub alignment: HorizontalAlignment,
}

impl TextRequest {
    /// Reject invalid or excessive work before scanning fonts or allocating pixels.
    ///
    /// # Errors
    /// Returns [`TextError`] for empty text, invalid dimensions, or exceeded limits.
    pub fn validate(&self) -> Result<(), TextError> {
        if self.text.trim().is_empty() {
            return Err(TextError::Empty);
        }
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(TextError::TooLong);
        }
        if !(1..=512).contains(&self.font_size_px) {
            return Err(TextError::FontSize);
        }
        if self.font_family.trim().is_empty() || self.font_family.len() > 256 {
            return Err(TextError::FontFamily);
        }
        if !(1..=MAX_TEXT_EDGE).contains(&self.size.width.get())
            || !(1..=MAX_TEXT_EDGE).contains(&self.size.height.get())
        {
            return Err(TextError::Dimensions);
        }
        if self.foreground.alpha == 0 {
            return Err(TextError::Invisible);
        }
        Ok(())
    }
}

/// Rasterized straight-alpha RGBA8 pixels suitable for an immutable overlay asset.
#[derive(Clone, Debug)]
pub struct TextImage {
    /// Exact text box dimensions.
    pub size: PhysicalSize,
    /// Row-major RGBA8 bytes.
    pub rgba: Vec<u8>,
}

/// Text failures leave the project unchanged.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TextError {
    /// Whitespace alone cannot form a caption.
    #[error("Enter some text first.")]
    Empty,
    /// Input exceeds the shaping budget.
    #[error("Text must be at most 16384 UTF-8 bytes.")]
    TooLong,
    /// A pathological font size was rejected.
    #[error("Font size must be between 1 and 512 pixels.")]
    FontSize,
    /// Font family is invalid.
    #[error("Enter a font family of at most 256 bytes.")]
    FontFamily,
    /// A requested explicit family is unavailable; fallback across scripts is still allowed.
    #[error("The selected font family is not installed. Choose sans-serif or an installed family.")]
    FontNotFound,
    /// Very large glyphs or too many glyphs exceed the rasterizer work budget.
    #[error("This text needs too much rasterization memory. Use less text or a smaller font size.")]
    GlyphBudget,
    /// Raster dimensions exceed the allocation budget.
    #[error("The text box must be between 1 and 4096 pixels in each dimension.")]
    Dimensions,
    /// Invisible captions are not useful authoring results.
    #[error("Text color must not be fully transparent.")]
    Invisible,
    /// Font fallback could not supply all visible glyphs.
    #[error(
        "Some characters have no installed font. Install a font for this script and try again."
    )]
    MissingGlyphs,
    /// Text did not fit inside its chosen box.
    #[error("The text does not fit. Increase the box size or reduce the font size.")]
    DoesNotFit,
    /// Pixel allocation failed.
    #[error("Could not allocate the text image.")]
    Allocation,
}

/// A reusable shaping context. Do not construct or rasterize it on the UI thread.
pub struct TextRasterizer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl TextRasterizer {
    /// Load installed fonts and bundled baseline fonts.
    pub fn with_system_fonts() -> Self {
        Self::from_fonts(FontSystem::new())
    }

    /// Use only bundled fonts, for reproducible tests or restricted environments.
    pub fn bundled_only() -> Self {
        Self::from_fonts(FontSystem::new_with_locale_and_db(
            "en-US".to_owned(),
            cosmic_text::fontdb::Database::new(),
        ))
    }

    fn from_fonts(mut fonts: FontSystem) -> Self {
        let db = fonts.db_mut();
        db.load_font_data(epaint_default_fonts::UBUNTU_LIGHT.to_vec());
        db.load_font_data(epaint_default_fonts::HACK_REGULAR.to_vec());
        db.load_font_data(epaint_default_fonts::NOTO_EMOJI_REGULAR.to_vec());
        db.set_sans_serif_family("Ubuntu");
        db.set_monospace_family("Hack");
        Self {
            fonts,
            cache: SwashCache::new(),
        }
    }

    /// Shape, wrap, align, and antialias text into a fixed-size straight-alpha image.
    ///
    /// # Errors
    /// Returns [`TextError`] for invalid input, missing glyphs, clipped text, or allocation failure.
    #[allow(
        clippy::cast_precision_loss,
        reason = "validated dimensions are at most 4096, exactly representable in f32"
    )]
    pub fn rasterize(&mut self, request: &TextRequest) -> Result<TextImage, TextError> {
        request.validate()?;
        let family_name = request.font_family.trim();
        if !["sans-serif", "serif", "monospace"].contains(&family_name)
            && !self.fonts.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family_name))
            })
        {
            return Err(TextError::FontNotFound);
        }
        // Bound accumulated glyph bitmaps across repeated edits as well as this individual call.
        self.cache = SwashCache::new();
        let width = request.size.width.get();
        let height = request.size.height.get();
        let font_size = f32::from(request.font_size_px);
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(font_size, font_size * 1.25));
        // Lay out the entire bounded input so overflow is reported, not silently clipped.
        buffer.set_size(&mut self.fonts, Some(width as f32), None);
        buffer.set_wrap(&mut self.fonts, Wrap::WordOrGlyph);
        let family = match request.font_family.trim() {
            "sans-serif" => Family::SansSerif,
            "serif" => Family::Serif,
            "monospace" => Family::Monospace,
            name => Family::Name(name),
        };
        buffer.set_text(
            &mut self.fonts,
            &request.text,
            &Attrs::new().family(family),
            Shaping::Advanced,
        );
        let align = match request.alignment {
            HorizontalAlignment::Start => Align::Left,
            HorizontalAlignment::Center => Align::Center,
            HorizontalAlignment::End => Align::Right,
        };
        for line in &mut buffer.lines {
            line.set_align(Some(align));
        }
        buffer.shape_until_scroll(&mut self.fonts, true);
        let mut glyph_pixels = 0_u64;
        for run in buffer.layout_runs() {
            glyph_pixels += run.glyphs.len() as u64 * u64::from(request.font_size_px).pow(2);
            if glyph_pixels > 32 * 1024 * 1024 {
                return Err(TextError::GlyphBudget);
            }
            if run.line_top + run.line_height > height as f32 || run.line_w > width as f32 + 0.01 {
                return Err(TextError::DoesNotFit);
            }
            if run.glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
                return Err(TextError::MissingGlyphs);
            }
        }
        let byte_len = usize::try_from(u64::from(width) * u64::from(height) * 4)
            .map_err(|_| TextError::Allocation)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(byte_len)
            .map_err(|_| TextError::Allocation)?;
        let background = request.background.map_or([0; 4], |color| {
            if color.alpha == 0 {
                [0; 4]
            } else {
                [color.red, color.green, color.blue, color.alpha]
            }
        });
        for _ in 0..byte_len / 4 {
            rgba.extend_from_slice(&background);
        }
        let color = request.foreground;
        let mut ink = false;
        let mut clipped = false;
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            Color::rgba(color.red, color.green, color.blue, color.alpha),
            |x, y, _, _, color| {
                if color.a() == 0 {
                    return;
                }
                let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                    clipped = true;
                    return;
                };
                if x >= width || y >= height {
                    clipped = true;
                    return;
                }
                ink = true;
                let index = (y as usize * width as usize + x as usize) * 4;
                source_over(&mut rgba[index..index + 4], color);
            },
        );
        if clipped || !ink {
            return Err(TextError::DoesNotFit);
        }
        Ok(TextImage {
            size: request.size,
            rgba,
        })
    }
}

fn source_over(destination: &mut [u8], source: Color) {
    let alpha = u32::from(source.a());
    let old_alpha = u32::from(destination[3]);
    let output_alpha = alpha * 255 + old_alpha * (255 - alpha);
    for (index, color) in [source.r(), source.g(), source.b()].into_iter().enumerate() {
        let numerator = u32::from(color) * alpha * 255
            + u32::from(destination[index]) * old_alpha * (255 - alpha);
        destination[index] = u8::try_from((numerator + output_alpha / 2) / output_alpha)
            .expect("source-over channels remain within u8");
    }
    destination[3] = u8::try_from((output_alpha + 127) / 255).expect("bounded source-over alpha");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> TextRequest {
        TextRequest {
            text: text.to_owned(),
            font_family: "monospace".to_owned(),
            font_size_px: 20,
            size: PhysicalSize::new(200, 100).unwrap(),
            foreground: Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            background: None,
            alignment: HorizontalAlignment::Start,
        }
    }

    #[test]
    fn text_is_antialiased_straight_alpha_and_reproducible() {
        let mut renderer = TextRasterizer::bundled_only();
        let first = renderer.rasterize(&request("Hello\nGIF")).unwrap();
        let second = renderer.rasterize(&request("Hello\nGIF")).unwrap();
        assert_eq!(first.rgba, second.rgba);
        assert!(
            first
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[3] < 128)
        );
        assert!(
            first
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 0 || p[0] == 255)
        );
    }

    #[test]
    fn alignment_and_background_are_baked_into_pixels() {
        let mut renderer = TextRasterizer::bundled_only();
        let mut input = request("Hi");
        let left = renderer.rasterize(&input).unwrap();
        input.alignment = HorizontalAlignment::End;
        let right = renderer.rasterize(&input).unwrap();
        assert_ne!(left.rgba, right.rgba);
        input.background = Some(Rgba {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 255,
        });
        let opaque = renderer.rasterize(&input).unwrap();
        assert!(opaque.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn overflow_and_missing_glyphs_are_explicit() {
        let mut renderer = TextRasterizer::bundled_only();
        let mut input = request("Too many\nlines");
        input.size = PhysicalSize::new(200, 20).unwrap();
        assert_eq!(
            renderer.rasterize(&input).unwrap_err(),
            TextError::DoesNotFit
        );
        assert_eq!(
            renderer.rasterize(&request("\u{10ffff}")).unwrap_err(),
            TextError::MissingGlyphs
        );
    }

    #[test]
    fn input_limits_precede_font_work() {
        assert_eq!(request(" ").validate(), Err(TextError::Empty));
        assert_eq!(
            request(&"x".repeat(MAX_TEXT_BYTES + 1)).validate(),
            Err(TextError::TooLong)
        );
        let mut input = request("Hi");
        input.font_size_px = 0;
        assert_eq!(input.validate(), Err(TextError::FontSize));
        input.font_size_px = 20;
        input.size.width = gif_from_screen_domain::PhysicalPx::new(MAX_TEXT_EDGE + 1);
        assert_eq!(input.validate(), Err(TextError::Dimensions));
    }

    #[test]
    fn unknown_explicit_font_does_not_silently_substitute() {
        let mut input = request("Hi");
        input.font_family = "not-a-real-font-942793".to_owned();
        assert_eq!(
            TextRasterizer::bundled_only()
                .rasterize(&input)
                .unwrap_err(),
            TextError::FontNotFound
        );
    }
}
