use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{LanguageTag, LanguageTagError};

/// Current application language-preference wire version, independent of projects.
pub const PREFERENCES_FORMAT_VERSION: u16 = 1;

/// System negotiation or an explicit, validated language identity.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "mode",
    content = "tag",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum LanguagePreference {
    /// Re-evaluate the supplied system locale on each application startup.
    #[default]
    System,
    /// Retain this choice even when its catalog is not currently available.
    Explicit(LanguageTag),
}

impl LanguagePreference {
    /// Construct an explicit preference without changing its spelling.
    ///
    /// # Errors
    /// Rejects malformed or oversized tags.
    pub fn explicit(tag: impl Into<String>) -> Result<Self, LanguageTagError> {
        LanguageTag::new(tag).map(Self::Explicit)
    }
}

/// Language-only application preferences. Persistence I/O belongs to the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Preferences {
    /// System by default, or the user's preserved explicit override.
    pub language: LanguagePreference,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WirePreferences {
    format_version: u16,
    language: LanguagePreference,
}

impl Serialize for Preferences {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WirePreferences {
            format_version: PREFERENCES_FORMAT_VERSION,
            language: self.language.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Preferences {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WirePreferences::deserialize(deserializer)?;
        if wire.format_version != PREFERENCES_FORMAT_VERSION {
            return Err(D::Error::custom(
                "Unsupported language preferences format version",
            ));
        }
        Ok(Self {
            language: wire.language,
        })
    }
}
