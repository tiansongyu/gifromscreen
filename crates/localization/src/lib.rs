#![forbid(unsafe_code)]

//! Locale-selection foundation, not a claim of translated or readable UI.
//!
//! The registry mirrors the 29 catalog identities in the pinned `ScreenToGif`
//! reference. It deliberately marks every catalog as not provided here. Actual
//! translations, font/shaping validation, persistence I/O and UI integration
//! belong to later layers. Locale resolution never reads or changes process
//! environment variables; callers supply an immutable environment snapshot.

mod preferences;
mod registry;
mod resolver;
mod tag;

pub use preferences::{LanguagePreference, PREFERENCES_FORMAT_VERSION, Preferences};
pub use registry::{Direction, LANGUAGE_REGISTRY, Language, TranslationReadiness, find_language};
pub use resolver::{
    FallbackReason, LinuxLocaleEnvironment, MAX_LANGUAGE_PREFERENCES, ResolutionSource,
    ResolvedLanguage, resolve_language,
};
pub use tag::{LanguageTag, LanguageTagError, MAX_LANGUAGE_TAG_BYTES};

#[cfg(test)]
mod tests;
