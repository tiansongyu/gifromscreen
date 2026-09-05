use gif_from_screen_application::{CustomGifPalette, CustomGifPaletteError};
use thiserror::Error;

const MIN_COLORS: usize = 2;
const MAX_COLORS: usize = 256;
const RGB_BYTES_PER_COLOR: usize = 3;

/// Strict UI-input failure for a custom GIF palette.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum CustomPaletteInputError {
    /// GIF palettes require at least two and at most 256 entries.
    #[error("custom palette must contain 2..=256 #RRGGBB entries, got {color_count}")]
    ColorCountOutOfRange { color_count: usize },
    /// Scanning stopped as soon as a 257th non-empty entry was observed.
    #[error("custom palette must contain at most 256 #RRGGBB entries, got at least 257")]
    TooManyColors,
    /// One non-empty token did not have the required hash-prefixed RGB shape.
    #[error("custom palette entry {entry} must be exactly #RRGGBB, got '{token}'")]
    InvalidToken { entry: usize, token: String },
    /// One RGB token contained a non-hexadecimal digit.
    #[error("custom palette entry {entry} contains invalid hexadecimal digit '{digit}'")]
    InvalidHexDigit { entry: usize, digit: char },
    /// The optional transparent index must fit both `u8` and the supplied palette.
    #[error("custom palette transparent index {index} is outside the {color_count}-color palette")]
    TransparentIndexOutOfRange { index: u16, color_count: usize },
    /// The bounded packed RGB buffer could not be reserved.
    #[error("could not reserve {color_count} custom palette colors")]
    AllocationFailed { color_count: usize },
    /// The application-level palette invariant unexpectedly rejected parsed input.
    #[error("custom palette validation failed: {0}")]
    Validation(#[source] CustomGifPaletteError),
}

/// Parses comma or whitespace separated `#RRGGBB` entries in stable index order.
///
/// Empty separator runs and trailing separators are ignored. Every non-empty
/// token remains strict: shorthand colors, missing `#`, alpha components, and
/// non-hexadecimal digits are rejected. Duplicate RGB entries are preserved.
///
/// # Errors
///
/// Returns [`CustomPaletteInputError`] for an invalid count/token/index,
/// bounded allocation failure, or an unexpected application invariant error.
pub(crate) fn parse_custom_palette(
    input: &str,
    transparent_index: Option<u16>,
) -> Result<CustomGifPalette, CustomPaletteInputError> {
    let color_count = tokens(input).take(MAX_COLORS + 1).count();
    if color_count < MIN_COLORS {
        return Err(CustomPaletteInputError::ColorCountOutOfRange { color_count });
    }
    if color_count > MAX_COLORS {
        return Err(CustomPaletteInputError::TooManyColors);
    }
    let transparent_index = transparent_index
        .map(|index| {
            let converted = u8::try_from(index).map_err(|_| {
                CustomPaletteInputError::TransparentIndexOutOfRange { index, color_count }
            })?;
            if usize::from(converted) >= color_count {
                return Err(CustomPaletteInputError::TransparentIndexOutOfRange {
                    index,
                    color_count,
                });
            }
            Ok(converted)
        })
        .transpose()?;
    let mut packed_rgb = Vec::new();
    packed_rgb
        .try_reserve_exact(color_count * RGB_BYTES_PER_COLOR)
        .map_err(|_| CustomPaletteInputError::AllocationFailed { color_count })?;
    for (entry_index, token) in tokens(input).enumerate() {
        parse_token(token, entry_index + 1, &mut packed_rgb)?;
    }
    CustomGifPalette::new(packed_rgb, transparent_index)
        .map_err(CustomPaletteInputError::Validation)
}

fn tokens(input: &str) -> impl Iterator<Item = &str> {
    input
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|token| !token.is_empty())
}

fn parse_token(
    token: &str,
    entry: usize,
    output: &mut Vec<u8>,
) -> Result<(), CustomPaletteInputError> {
    let bytes = token.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return Err(CustomPaletteInputError::InvalidToken {
            entry,
            token: token.to_owned(),
        });
    }
    for pair in bytes[1..].as_chunks::<2>().0 {
        let high =
            hex_nibble(pair[0]).map_err(|digit| CustomPaletteInputError::InvalidHexDigit {
                entry,
                digit: char::from(digit),
            })?;
        let low =
            hex_nibble(pair[1]).map_err(|digit| CustomPaletteInputError::InvalidHexDigit {
                entry,
                digit: char::from(digit),
            })?;
        output.push((high << 4) | low);
    }
    Ok(())
}

const fn hex_nibble(digit: u8) -> Result<u8, u8> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => Err(digit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whitespace_commas_newlines_case_duplicates_and_transparency() {
        let palette =
            parse_custom_palette("  #ff0000, #00FF00\n\t#0000fF   #ff0000, ", Some(3)).unwrap();
        assert_eq!(palette.color_count(), 4);
        assert_eq!(
            palette.packed_rgb(),
            [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 0]
        );
        assert_eq!(palette.transparent_index(), Some(3));
        assert_eq!(
            parse_custom_palette("#000000 #FFFFFF", None)
                .unwrap()
                .transparent_index(),
            None
        );
    }

    #[test]
    fn accepts_exactly_two_and_256_entries() {
        assert_eq!(
            parse_custom_palette("#000000,#ffffff", None)
                .unwrap()
                .color_count(),
            2
        );
        let maximum = (0..256)
            .map(|value| format!("#{value:02X}0000"))
            .collect::<Vec<_>>()
            .join("\n");
        let palette = parse_custom_palette(&maximum, Some(255)).unwrap();
        assert_eq!(palette.color_count(), 256);
        assert_eq!(palette.transparent_index(), Some(255));
    }

    #[test]
    fn rejects_counts_malformed_tokens_and_bad_hex_strictly() {
        for input in ["", " , \n", "#000000"] {
            assert!(matches!(
                parse_custom_palette(input, None),
                Err(CustomPaletteInputError::ColorCountOutOfRange { .. })
            ));
        }
        let too_many = std::iter::repeat_n("#000000", 257)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(matches!(
            parse_custom_palette(&too_many, None),
            Err(CustomPaletteInputError::TooManyColors)
        ));
        let malformed_tail = format!("{} definitely-not-a-color", "#000000 ".repeat(257));
        assert_eq!(
            parse_custom_palette(&malformed_tail, None),
            Err(CustomPaletteInputError::TooManyColors)
        );
        for token in ["000000", "#fff", "#00000000", "#💙0000"] {
            assert!(matches!(
                parse_custom_palette(&format!("#000000 {token}"), None),
                Err(CustomPaletteInputError::InvalidToken { entry: 2, .. })
            ));
        }
        assert!(matches!(
            parse_custom_palette("#000000 #12GG56", None),
            Err(CustomPaletteInputError::InvalidHexDigit {
                entry: 2,
                digit: 'G'
            })
        ));
        assert!(matches!(
            parse_custom_palette("#000000;#FFFFFF", None),
            Err(CustomPaletteInputError::ColorCountOutOfRange { color_count: 1 })
        ));
    }

    #[test]
    fn transparent_index_must_name_an_existing_entry() {
        for (index, color_count) in [(2, 2), (255, 2), (256, 2)] {
            assert_eq!(
                parse_custom_palette("#000000 #FFFFFF", Some(index)),
                Err(CustomPaletteInputError::TransparentIndexOutOfRange { index, color_count })
            );
        }
    }
}
