//! Bounded immutable input-history references for editable frame-owned annotations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AssetId, CaptureClockId, CaptureOrigin, FrameAuthoringSpan, KeyStroke, MouseInputEvent, TimeUs,
};

pub const INPUT_REPLAY_MEDIA_TYPE: &str = "application/vnd.gifromscreen.input-replay+json";
pub const MAX_INPUT_REPLAY_POOL_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_INPUT_REPLAY_EVENTS: usize = 65_536;
pub const MAX_INPUT_REPLAY_RUNS_PER_CELL: usize = 64;

/// Each reference is a prefix of one continuous source-clock/authoring-run pool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameInputReplay {
    pub runs: Vec<FrameInputReplayRef>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameInputReplayRef {
    pub run_id: u32,
    pub asset_id: AssetId,
    /// Source sampling instant, independent of the edited playback timeline.
    pub sample_at: TimeUs,
    /// Exclusive event-bearing step count; later pool steps never contribute.
    pub step_end: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameInputReplayPool {
    pub version: u32,
    pub clock_id: Option<CaptureClockId>,
    pub started_at: TimeUs,
    pub steps: Vec<InputReplayStep>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputReplayStep {
    /// Delivery/sample time, not the earlier native event timestamp.
    pub sample_at: TimeUs,
    pub capture_origin: Option<CaptureOrigin>,
    pub keys: Vec<KeyStroke>,
    pub mouse_events: Vec<MouseInputEvent>,
}

impl FrameInputReplay {
    pub fn validate(&self, scopes: &[FrameAuthoringSpan]) -> Result<(), String> {
        if self.runs.is_empty() || self.runs.len() > MAX_INPUT_REPLAY_RUNS_PER_CELL {
            return Err("Frame input replay requires 1–64 isolated authoring runs.".to_owned());
        }
        let required: BTreeSet<_> = scopes.iter().map(|scope| scope.run_id).collect();
        let mut known = BTreeSet::new();
        for run in &self.runs {
            if run.run_id == 0
                || !known.insert(run.run_id)
                || run.step_end as usize > MAX_INPUT_REPLAY_EVENTS
            {
                return Err("Frame input replay has an invalid run or event prefix.".to_owned());
            }
        }
        if known != required {
            return Err("Frame input replay must cover exactly its authoring runs.".to_owned());
        }
        Ok(())
    }

    pub fn referenced_assets(&self) -> impl Iterator<Item = AssetId> + '_ {
        self.runs.iter().map(|run| run.asset_id)
    }
}

impl FrameInputReplayPool {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.clock_id.is_some_and(CaptureClockId::is_nil) {
            return Err(
                "Unknown input replay version or invalid source clock identity.".to_owned(),
            );
        }
        let mut previous = None;
        let mut events = 0_usize;
        for step in &self.steps {
            if step.sample_at < self.started_at
                || previous.is_some_and(|at| step.sample_at <= at)
                || step.keys.len() > 512
                || step.mouse_events.len() > 512
                || step.keys.is_empty() && step.mouse_events.is_empty()
            {
                return Err(
                    "Input replay contains unordered, empty or oversized event steps.".to_owned(),
                );
            }
            previous = Some(step.sample_at);
            events = events
                .checked_add(step.keys.len())
                .and_then(|n| n.checked_add(step.mouse_events.len()))
                .filter(|n| *n <= MAX_INPUT_REPLAY_EVENTS)
                .ok_or("Input replay exceeds 65,536 events.")?;
            if step.keys.iter().any(|key| {
                key.physical_key.len() > 4096
                    || key
                        .display_text
                        .as_ref()
                        .is_some_and(|text| text.len() > 4096)
            }) {
                return Err("Input replay key text exceeds 4096 bytes.".to_owned());
            }
        }
        if self.clock_id.is_none()
            && (self.steps.len() > 1
                || self
                    .steps
                    .first()
                    .is_some_and(|step| step.sample_at != self.started_at))
        {
            return Err("An unknown clock cannot carry input between samples.".to_owned());
        }
        Ok(())
    }

    /// Checks one owner prefix after the shared pool has passed [`Self::validate`].
    pub fn validate_reference(&self, reference: &FrameInputReplayRef) -> Result<(), String> {
        // Validate the shared pool once before checking its individual prefixes.
        let count =
            usize::try_from(reference.step_end).map_err(|_| "Input replay prefix is too large.")?;
        if reference.sample_at < self.started_at
            || count > self.steps.len()
            || count
                .checked_sub(1)
                .and_then(|index| self.steps.get(index))
                .is_some_and(|step| step.sample_at > reference.sample_at)
            || self
                .steps
                .get(count)
                .is_some_and(|step| step.sample_at <= reference.sample_at)
            || self.clock_id.is_none() && reference.sample_at != self.started_at
        {
            return Err(
                "Input replay prefix does not match the owner's source sampling instant."
                    .to_owned(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameId, FrameLocalSpan, FrameOverlayCell};

    fn key(at: u64) -> KeyStroke {
        KeyStroke {
            physical_key: "KeyA".to_owned(),
            display_text: Some("A".to_owned()),
            pressed: true,
            at: TimeUs::new(at),
            repeat: false,
            modifiers: 0,
        }
    }
    fn pool() -> FrameInputReplayPool {
        FrameInputReplayPool {
            version: 1,
            clock_id: Some(CaptureClockId::from_u128(1)),
            started_at: TimeUs::ZERO,
            steps: vec![
                InputReplayStep {
                    sample_at: TimeUs::ZERO,
                    capture_origin: None,
                    keys: vec![key(0)],
                    mouse_events: Vec::new(),
                },
                InputReplayStep {
                    sample_at: TimeUs::new(200),
                    capture_origin: None,
                    keys: vec![key(200)],
                    mouse_events: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn prefixes_are_source_timed_and_never_include_a_future_delivery_step() {
        let pool = pool();
        pool.validate().unwrap();
        let mut reference = FrameInputReplayRef {
            run_id: 1,
            asset_id: AssetId::from_digest([1; 32]),
            sample_at: TimeUs::new(100),
            step_end: 1,
        };
        pool.validate_reference(&reference).unwrap();
        for end in [0, 2, u32::MAX] {
            reference.step_end = end;
            assert!(pool.validate_reference(&reference).is_err());
        }
        reference.step_end = 2;
        reference.sample_at = TimeUs::new(200);
        pool.validate_reference(&reference).unwrap();
    }

    #[test]
    fn malformed_source_pools_and_unmatched_authoring_runs_are_rejected() {
        let mut source = pool();
        source.clock_id = None;
        assert!(source.validate().is_err());
        source.steps.truncate(1);
        source.validate().unwrap();
        source.steps[0].keys[0].physical_key = "A".repeat(4097);
        assert!(source.validate().is_err());
        source = pool();
        source.steps[1].sample_at = TimeUs::ZERO;
        assert!(source.validate().is_err());
        let reference = FrameInputReplayRef {
            run_id: 1,
            asset_id: AssetId::from_digest([1; 32]),
            sample_at: TimeUs::ZERO,
            step_end: 1,
        };
        let mut replay = FrameInputReplay {
            runs: vec![reference],
        };
        let scopes = [FrameAuthoringSpan {
            run_id: 1,
            span: FrameLocalSpan::WHOLE,
        }];
        replay.validate(&scopes).unwrap();
        replay.runs.push(reference);
        assert!(replay.validate(&scopes).is_err());
        replay.runs = vec![FrameInputReplayRef {
            run_id: 2,
            ..reference
        }];
        assert!(replay.validate(&scopes).is_err());
    }

    #[test]
    fn old_frozen_cells_deserialize_without_claiming_missing_replay_history() {
        let cell = FrameOverlayCell {
            frame_id: FrameId::from_u128(1),
            scopes: vec![FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::WHOLE,
            }],
            marks: Vec::new(),
            input_replay: None,
        };
        let json = serde_json::to_string(&cell).unwrap();
        assert!(!json.contains("input_replay"));
        assert_eq!(
            serde_json::from_str::<FrameOverlayCell>(&json).unwrap(),
            cell
        );
    }
}
