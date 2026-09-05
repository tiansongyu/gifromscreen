use std::{error::Error, fmt, io, path::PathBuf};

use gif_from_screen_domain::{AssetId, DomainError, ProjectRevision};

#[derive(Debug)]
pub enum ProjectError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json {
        operation: &'static str,
        path: PathBuf,
        source: serde_json::Error,
    },
    Domain(DomainError),
    AlreadyLocked {
        path: PathBuf,
        owner: Option<String>,
    },
    ManifestAlreadyExists(PathBuf),
    MissingManifest(PathBuf),
    JournalCommitFailed {
        revision: ProjectRevision,
        source: Box<ProjectError>,
    },
    RequiresRecovery,
    RequiresJournalRepair,
    CorruptAsset {
        asset_id: AssetId,
        path: PathBuf,
    },
    AssetLengthMismatch {
        asset_id: AssetId,
        expected: u64,
        actual: u64,
    },
    InvalidJournalRecord(String),
    RecordingIndexAllocationFailed {
        requested: usize,
    },
    InvalidRecordingMutation(String),
}

impl ProjectError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn json(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: serde_json::Error,
    ) -> Self {
        Self::Json {
            operation,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Json {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Domain(source) => source.fmt(formatter),
            Self::AlreadyLocked { path, owner } => {
                write!(formatter, "project {} is already locked", path.display())?;
                if let Some(owner) = owner {
                    write!(formatter, " by {owner}")?;
                }
                Ok(())
            }
            Self::ManifestAlreadyExists(path) => {
                write!(
                    formatter,
                    "project manifest already exists at {}",
                    path.display()
                )
            }
            Self::MissingManifest(path) => {
                write!(
                    formatter,
                    "project manifest is missing at {}",
                    path.display()
                )
            }
            Self::JournalCommitFailed { revision, source } => {
                write!(
                    formatter,
                    "journal commit for revision {revision} is ambiguous: {source}"
                )
            }
            Self::RequiresRecovery => formatter
                .write_str("a previous journal write failed; close and recover the project"),
            Self::RequiresJournalRepair => formatter
                .write_str("journal has an invalid tail; repair it before appending more commands"),
            Self::CorruptAsset { asset_id, path } => {
                write!(
                    formatter,
                    "asset {asset_id} does not match its digest at {}",
                    path.display()
                )
            }
            Self::AssetLengthMismatch {
                asset_id,
                expected,
                actual,
            } => write!(
                formatter,
                "asset {asset_id} has {actual} bytes, expected {expected}"
            ),
            Self::InvalidJournalRecord(reason) => {
                write!(formatter, "invalid journal record: {reason}")
            }
            Self::RecordingIndexAllocationFailed { requested } => write!(
                formatter,
                "could not allocate the recording frame index for {requested} entries"
            ),
            Self::InvalidRecordingMutation(reason) => {
                write!(
                    formatter,
                    "invalid incremental recording mutation: {reason}"
                )
            }
        }
    }
}

impl Error for ProjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Domain(source) => Some(source),
            Self::JournalCommitFailed { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<DomainError> for ProjectError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value)
    }
}
