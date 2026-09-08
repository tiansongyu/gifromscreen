#![forbid(unsafe_code)]

//! Locale selection and an initial message set, not a complete translated UI.
//!
//! The registry mirrors the 29 catalog identities in the pinned `ScreenToGif`
//! reference. Whole-UI readiness is still not provided; the separate catalog
//! exposes an initial English/Simplified Chinese slice and honest coverage.
//! Full translation, font/shaping validation, persistence I/O and UI integration
//! remain separate work. Locale resolution never reads or changes process
//! environment variables; callers supply an immutable environment snapshot.

mod catalog;
mod preferences;
mod registry;
mod resolver;
mod tag;

pub use catalog::{
    ALL_MESSAGES, CatalogCoverage, CatalogScope, CatalogSource, FormatError, LocalizedText,
    Localizer, MAX_FORMATTED_MESSAGE_BYTES, Message, catalog_coverage,
};
pub use preferences::{LanguagePreference, PREFERENCES_FORMAT_VERSION, Preferences};
pub use registry::{Direction, LANGUAGE_REGISTRY, Language, TranslationReadiness, find_language};
pub use resolver::{
    FallbackReason, LinuxLocaleEnvironment, MAX_LANGUAGE_PREFERENCES, ResolutionSource,
    ResolvedLanguage, resolve_language,
};
pub use tag::{LanguageTag, LanguageTagError, MAX_LANGUAGE_TAG_BYTES};

#[cfg(test)]
mod tests;
