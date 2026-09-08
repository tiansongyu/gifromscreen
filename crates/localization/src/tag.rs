use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

/// Maximum stored language-tag length, in ASCII bytes.
pub const MAX_LANGUAGE_TAG_BYTES: usize = 63;

/// Failure to accept a bounded, structurally well-formed BCP 47 tag.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LanguageTagError {
    /// Empty strings are not explicit language preferences.
    #[error("An explicit language tag must not be empty")]
    Empty,
    /// The tag exceeded the preference storage budget.
    #[error("Language tags must not exceed {maximum} bytes")]
    TooLong {
        /// Maximum accepted byte length.
        maximum: usize,
    },
    /// Invalid syntax, including whitespace, POSIX notation or duplicate subtags.
    #[error("Expected a well-formed BCP 47 language tag")]
    Invalid,
}

/// Validated language preference, preserving its original spelling and case.
///
/// Structural validation does not require an entry in the current IANA registry
/// or in our language list. A valid unavailable choice therefore survives save,
/// reopen and future catalog additions. POSIX locale conversion is only applied
/// to injected system environment values, never to explicitly stored tags.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Validate and retain a tag without changing its spelling.
    ///
    /// # Errors
    /// Rejects empty, oversized or structurally malformed tags.
    pub fn new(tag: impl Into<String>) -> Result<Self, LanguageTagError> {
        let tag = tag.into();
        if tag.is_empty() {
            return Err(LanguageTagError::Empty);
        }
        if tag.len() > MAX_LANGUAGE_TAG_BYTES {
            return Err(LanguageTagError::TooLong {
                maximum: MAX_LANGUAGE_TAG_BYTES,
            });
        }
        if !well_formed(&tag) {
            return Err(LanguageTagError::Invalid);
        }
        Ok(Self(tag))
    }

    /// The original validated tag, including unavailable language choices.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for LanguageTag {
    type Err = LanguageTagError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for LanguageTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

// RFC 5646's fixed grandfathered set, accepted as identities, not silently
// aliased to other catalogs. The general grammar below covers modern tags.
const GRANDFATHERED: &[&str] = &[
    "en-GB-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "sgn-BE-FR",
    "sgn-BE-NL",
    "sgn-CH-DE",
    "art-lojban",
    "cel-gaulish",
    "no-bok",
    "no-nyn",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-xiang",
];

fn alphabetic(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn well_formed(tag: &str) -> bool {
    if GRANDFATHERED
        .iter()
        .any(|entry| entry.eq_ignore_ascii_case(tag))
    {
        return true;
    }
    let parts: Vec<_> = tag.split('-').collect();
    if parts.iter().any(|part| {
        part.is_empty() || part.len() > 8 || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return false;
    }
    if parts[0].eq_ignore_ascii_case("x") {
        return parts.len() > 1;
    }
    if !(2..=8).contains(&parts[0].len()) || !alphabetic(parts[0]) {
        return false;
    }
    let mut index = 1;
    if parts[0].len() <= 3 {
        for _ in 0..3 {
            if parts
                .get(index)
                .is_some_and(|part| part.len() == 3 && alphabetic(part))
            {
                index += 1;
            } else {
                break;
            }
        }
    }
    if parts
        .get(index)
        .is_some_and(|part| part.len() == 4 && alphabetic(part))
    {
        index += 1;
    }
    if parts.get(index).is_some_and(|part| {
        (part.len() == 2 && alphabetic(part))
            || (part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        index += 1;
    }
    let mut variants: Vec<&str> = Vec::new();
    while let Some(part) = parts.get(index).copied().filter(|part| {
        (5..=8).contains(&part.len()) || (part.len() == 4 && part.as_bytes()[0].is_ascii_digit())
    }) {
        if variants.iter().any(|old| old.eq_ignore_ascii_case(part)) {
            return false;
        }
        variants.push(part);
        index += 1;
    }
    extensions_well_formed(&parts[index..])
}

fn extensions_well_formed(parts: &[&str]) -> bool {
    let mut index = 0;
    let mut singletons = Vec::new();
    while let Some(part) = parts.get(index) {
        if part.eq_ignore_ascii_case("x") {
            return index + 1 < parts.len();
        }
        if part.len() != 1 {
            return false;
        }
        let singleton = part.as_bytes()[0].to_ascii_lowercase();
        if singletons.contains(&singleton) {
            return false;
        }
        singletons.push(singleton);
        index += 1;
        let beginning = index;
        while parts.get(index).is_some_and(|part| part.len() >= 2) {
            index += 1;
        }
        if index == beginning {
            return false;
        }
    }
    true
}
