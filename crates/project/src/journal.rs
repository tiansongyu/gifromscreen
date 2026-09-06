use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
};

use gif_from_screen_domain::{EditCommand, ProjectManifest, ProjectRevision};
use serde::{Deserialize, Serialize};

use crate::ProjectError;

pub const JOURNAL_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub format_version: u32,
    /// Sequence is deliberately equal to `result_revision`, so a compacted
    /// journal does not require a second persisted counter.
    pub sequence: u64,
    pub base_revision: ProjectRevision,
    pub result_revision: ProjectRevision,
    pub command: EditCommand,
    pub checksum: String,
}

#[derive(Serialize)]
struct ChecksumPayload<'a> {
    format_version: u32,
    sequence: u64,
    base_revision: ProjectRevision,
    result_revision: ProjectRevision,
    command: &'a EditCommand,
}

impl JournalRecord {
    pub fn new(
        base_revision: ProjectRevision,
        result_revision: ProjectRevision,
        command: EditCommand,
    ) -> Result<Self, ProjectError> {
        if base_revision.next() != Some(result_revision) {
            return Err(ProjectError::InvalidJournalRecord(format!(
                "result revision {result_revision} does not immediately follow {base_revision}"
            )));
        }
        let mut record = Self {
            format_version: JOURNAL_FORMAT_VERSION,
            sequence: result_revision.get(),
            base_revision,
            result_revision,
            command,
            checksum: String::new(),
        };
        record.checksum = record.expected_checksum()?;
        Ok(record)
    }

    pub fn verify(&self) -> Result<(), ProjectError> {
        if self.format_version != JOURNAL_FORMAT_VERSION {
            return Err(ProjectError::InvalidJournalRecord(format!(
                "unsupported journal format {}",
                self.format_version
            )));
        }
        if self.base_revision.next() != Some(self.result_revision) {
            return Err(ProjectError::InvalidJournalRecord(format!(
                "revision {} does not immediately follow {}",
                self.result_revision, self.base_revision
            )));
        }
        if self.sequence != self.result_revision.get() {
            return Err(ProjectError::InvalidJournalRecord(format!(
                "sequence {} differs from result revision {}",
                self.sequence, self.result_revision
            )));
        }
        if self.checksum != self.expected_checksum()? {
            return Err(ProjectError::InvalidJournalRecord(
                "checksum mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    fn expected_checksum(&self) -> Result<String, ProjectError> {
        let payload = ChecksumPayload {
            format_version: self.format_version,
            sequence: self.sequence,
            base_revision: self.base_revision,
            result_revision: self.result_revision,
            command: &self.command,
        };
        let bytes = serde_json::to_vec(&payload).map_err(|error| {
            ProjectError::json(
                "serialize journal checksum payload",
                "journal.ndjson",
                error,
            )
        })?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JournalStopReason {
    InvalidJson {
        line: u64,
        message: String,
    },
    InvalidEnvelope {
        line: u64,
        message: String,
    },
    /// New payload appeared before its required schema was durably stamped.
    SchemaMismatch {
        line: u64,
        snapshot_schema: u32,
        required_schema: u32,
    },
    NonMonotonicRevision {
        line: u64,
        previous: ProjectRevision,
        found: ProjectRevision,
    },
    RevisionGap {
        line: u64,
        expected_base: ProjectRevision,
        found_base: ProjectRevision,
    },
    CommandRejected {
        line: u64,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecoveryReport {
    pub snapshot_revision: ProjectRevision,
    pub recovered_revision: ProjectRevision,
    pub replayed_records: u64,
    pub already_snapshotted_records: u64,
    pub stop_reason: Option<JournalStopReason>,
}

impl JournalRecoveryReport {
    pub fn is_clean(&self) -> bool {
        self.stop_reason.is_none()
    }
}

pub(crate) struct RecoveredJournal {
    pub manifest: ProjectManifest,
    pub report: JournalRecoveryReport,
}

pub(crate) fn append(path: &Path, record: &JournalRecord) -> Result<(), ProjectError> {
    record.verify()?;
    let mut bytes = serde_json::to_vec(record)
        .map_err(|error| ProjectError::json("serialize journal record", path, error))?;
    bytes.push(b'\n');

    let mut journal = crate::private_fs::file_options()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| ProjectError::io("open journal for append", path, error))?;
    journal
        .write_all(&bytes)
        .map_err(|error| ProjectError::io("append journal record", path, error))?;
    journal
        .sync_data()
        .map_err(|error| ProjectError::io("sync journal record", path, error))
}

pub(crate) fn recover(
    mut manifest: ProjectManifest,
    path: &Path,
) -> Result<RecoveredJournal, ProjectError> {
    manifest.validate()?;
    let snapshot_schema = manifest.schema_version;
    let snapshot_revision = manifest.revision;
    let mut report = JournalRecoveryReport {
        snapshot_revision,
        recovered_revision: snapshot_revision,
        replayed_records: 0,
        already_snapshotted_records: 0,
        stop_reason: None,
    };
    if !path.exists() {
        return Ok(RecoveredJournal { manifest, report });
    }

    let file = fs::File::open(path)
        .map_err(|error| ProjectError::io("open journal for recovery", path, error))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut line_number = 0_u64;
    let mut previous_record_revision = None;

    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| ProjectError::io("read journal", path, error))?;
        if read == 0 {
            break;
        }
        line_number += 1;
        while buffer
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            buffer.pop();
        }

        let record: JournalRecord = match serde_json::from_slice(&buffer) {
            Ok(record) => record,
            Err(error) => {
                report.stop_reason = Some(JournalStopReason::InvalidJson {
                    line: line_number,
                    message: error.to_string(),
                });
                break;
            }
        };
        if let Err(error) = record.verify() {
            report.stop_reason = Some(JournalStopReason::InvalidEnvelope {
                line: line_number,
                message: error.to_string(),
            });
            break;
        }
        let required_schema = record.command.required_schema_version();
        if required_schema > snapshot_schema {
            report.stop_reason = Some(JournalStopReason::SchemaMismatch {
                line: line_number,
                snapshot_schema,
                required_schema,
            });
            break;
        }
        if let Some(previous) = previous_record_revision
            && record.result_revision <= previous
        {
            report.stop_reason = Some(JournalStopReason::NonMonotonicRevision {
                line: line_number,
                previous,
                found: record.result_revision,
            });
            break;
        }
        previous_record_revision = Some(record.result_revision);

        if record.result_revision <= manifest.revision {
            report.already_snapshotted_records += 1;
            continue;
        }
        if record.base_revision != manifest.revision {
            report.stop_reason = Some(JournalStopReason::RevisionGap {
                line: line_number,
                expected_base: manifest.revision,
                found_base: record.base_revision,
            });
            break;
        }

        match manifest.apply_command(&record.command) {
            Ok(applied) if applied.to_revision == record.result_revision => {
                report.replayed_records += 1;
                report.recovered_revision = manifest.revision;
            }
            Ok(_) => {
                report.stop_reason = Some(JournalStopReason::InvalidEnvelope {
                    line: line_number,
                    message: "domain revision differs from journal result revision".to_owned(),
                });
                break;
            }
            Err(error) => {
                report.stop_reason = Some(JournalStopReason::CommandRejected {
                    line: line_number,
                    message: error.to_string(),
                });
                break;
            }
        }
    }

    Ok(RecoveredJournal { manifest, report })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, ColorSpace, EditCommand,
        PhysicalSize, ProjectId, ProjectManifest, RasterEncoding, UnixTimeMs,
    };
    use tempfile::tempdir;

    use super::*;

    fn manifest() -> ProjectManifest {
        ProjectManifest::new(
            ProjectId::from_u128(1),
            "test",
            UnixTimeMs::new(1),
            Canvas {
                size: PhysicalSize::new(10, 10).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap()
    }

    fn command() -> EditCommand {
        let id = AssetId::from_digest([1; 32]);
        EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: 4,
                kind: AssetKind::Frame {
                    size: PhysicalSize::new(1, 1).unwrap(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        }
    }

    #[test]
    fn checksum_detects_payload_tampering() {
        let mut record =
            JournalRecord::new(ProjectRevision::ZERO, ProjectRevision::new(1), command()).unwrap();
        record.sequence = 2;
        assert!(record.verify().is_err());
    }

    #[test]
    fn recovery_applies_valid_prefix_and_stops_at_torn_tail() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("journal.ndjson");
        let record =
            JournalRecord::new(ProjectRevision::ZERO, ProjectRevision::new(1), command()).unwrap();
        append(&path, &record).unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"format_version":1,"sequence"#).unwrap();
        file.sync_all().unwrap();

        let recovered = recover(manifest(), &path).unwrap();
        assert_eq!(recovered.manifest.revision, ProjectRevision::new(1));
        assert_eq!(recovered.report.replayed_records, 1);
        assert!(matches!(
            recovered.report.stop_reason,
            Some(JournalStopReason::InvalidJson { line: 2, .. })
        ));
    }
}
