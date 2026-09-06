//! Bounded key-label history: collapse modifier prefixes, not distinct keystrokes.

use gif_from_screen_domain::KeyStroke;

const MAX_FRAME_EVENTS: usize = 512;
const MAX_ACTIVE_LABELS: usize = 256;
const MAX_LABEL_BYTES: usize = 4096;
const MODIFIERS: [(u8, &str); 4] = [(2, "Ctrl"), (4, "Alt"), (1, "Shift"), (8, "Super")];

#[derive(Clone, Debug, Eq, PartialEq)]
struct LabelIdentity {
    physical_key: String,
    text: String,
    modifiers: u8,
}

#[derive(Clone, Debug)]
struct TimedLabel {
    visible_at: u64,
    identity: LabelIdentity,
    modifier_only: bool,
    mergeable: bool,
}

#[derive(Clone, Debug)]
struct KeyLabel {
    identity: LabelIdentity,
    /// The last token is the key itself, rather than an added modifier prefix.
    own_modifier: u8,
    modifier_only: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KeyLabelHistory {
    labels: Vec<TimedLabel>,
    deduplicate_next: bool,
    last_clock: Option<u64>,
}

impl KeyLabelHistory {
    pub(crate) fn clear(&mut self) {
        self.labels.clear();
        self.deduplicate_next = false;
        self.last_clock = None;
    }

    /// Events retain their recorded order. A newly delivered sparse-sample event
    /// starts its hold at the first visible frame, not at its earlier capture time.
    /// Failure leaves the previous history unchanged.
    pub(crate) fn update(
        &mut self,
        events: &[KeyStroke],
        clock: u64,
        hold_ms: u32,
    ) -> Result<String, String> {
        if events.len() > MAX_FRAME_EVENTS {
            return Err(
                "A frame has more than 512 key events; reduce the recording event rate.".to_owned(),
            );
        }
        let mut next = self.clone();
        if next.last_clock.is_some_and(|previous| clock < previous) {
            next.clear();
        }
        let oldest = clock.saturating_sub(u64::from(hold_ms) * 1000);
        next.labels
            .retain(|label| label.visible_at >= oldest && label.visible_at <= clock);
        if next.labels.is_empty() {
            next.deduplicate_next = false;
        }
        for event in events.iter().filter(|event| event.at.get() <= clock) {
            if !event.pressed {
                next.release(event);
            } else if !event.repeat {
                next.press(event, clock)?;
            }
        }
        let text = next.text()?;
        next.last_clock = Some(clock);
        *self = next;
        Ok(text)
    }

    fn release(&mut self, event: &KeyStroke) {
        self.deduplicate_next = false;
        let own_modifier = key_label(event).map_or(0, |released| released.own_modifier);
        // Releasing one Ctrl must not end a chord while another Ctrl remains held.
        let inactive = own_modifier & !event.modifiers;
        for previous in &mut self.labels {
            if previous.modifier_only
                && (previous.identity.modifiers & inactive != 0
                    || (own_modifier == 0 && previous.identity.physical_key == event.physical_key))
            {
                previous.mergeable = false;
            }
        }
    }

    fn press(&mut self, event: &KeyStroke, clock: u64) -> Result<(), String> {
        let label = key_label(event)?;
        if label.identity.text.is_empty() {
            return Ok(());
        }
        if self.deduplicate_next
            && self
                .labels
                .last()
                .is_some_and(|previous| previous.identity == label.identity)
        {
            return Ok(());
        }
        // Only trailing modifier labels belong to the next combination. Earlier
        // independent labels and released modifier taps retain their own history.
        while self.labels.last().is_some_and(|previous| {
            previous.modifier_only
                && previous.mergeable
                && previous.identity.modifiers & label.identity.modifiers
                    == previous.identity.modifiers
        }) {
            self.labels.pop();
        }
        if self.labels.len() >= MAX_ACTIVE_LABELS {
            return Err("More than 256 simultaneous key labels; shorten the hold time.".to_owned());
        }
        self.deduplicate_next = true;
        self.labels.push(TimedLabel {
            visible_at: clock,
            identity: label.identity,
            modifier_only: label.modifier_only,
            mergeable: label.modifier_only,
        });
        Ok(())
    }

    fn text(&self) -> Result<String, String> {
        let mut text = String::new();
        for label in &self.labels {
            let separator = if text.is_empty() { "" } else { "  " };
            if text.len() + separator.len() + label.identity.text.len() > MAX_LABEL_BYTES {
                return Err(
                    "The combined recorded key label exceeds 4096 bytes; shorten the hold time."
                        .to_owned(),
                );
            }
            text.push_str(separator);
            text.push_str(&label.identity.text);
        }
        Ok(text)
    }
}

fn key_label(key: &KeyStroke) -> Result<KeyLabel, String> {
    if key.physical_key.len() > MAX_LABEL_BYTES {
        return Err("A recorded physical key name exceeds 4096 bytes.".to_owned());
    }
    let raw = key
        .display_text
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(&key.physical_key);
    if raw.is_empty() {
        return Ok(KeyLabel {
            identity: LabelIdentity {
                physical_key: String::new(),
                text: String::new(),
                modifiers: key.modifiers & 15,
            },
            own_modifier: 0,
            modifier_only: false,
        });
    }
    if raw.len() > MAX_LABEL_BYTES {
        return Err("A recorded key label exceeds 4096 bytes.".to_owned());
    }
    let mut remainder = match raw {
        " " => "Space",
        "\t" => "Tab",
        "\r" | "\n" | "\r\n" => "Enter",
        other => other,
    };
    if remainder.chars().any(char::is_control) {
        return Err("A recorded key label contains unsupported control characters.".to_owned());
    }
    let mut modifiers = key.modifiers & 15;
    while let Some((prefix, tail)) = remainder.split_once('+') {
        let Some(bit) = modifier_bit(prefix) else {
            break;
        };
        modifiers |= bit;
        remainder = tail;
    }
    let own_modifier = modifier_bit(remainder).unwrap_or(0);
    modifiers |= own_modifier;
    let modifier_only = own_modifier != 0;
    let mut parts: Vec<&str> = MODIFIERS
        .iter()
        .filter_map(|(bit, name)| (modifiers & bit != 0).then_some(*name))
        .collect();
    if !modifier_only && !remainder.is_empty() {
        parts.push(remainder);
    }
    let text = parts.join("+");
    if text.len() > MAX_LABEL_BYTES {
        return Err("A recorded key label exceeds 4096 bytes.".to_owned());
    }
    Ok(KeyLabel {
        identity: LabelIdentity {
            physical_key: key.physical_key.clone(),
            text,
            modifiers,
        },
        own_modifier,
        modifier_only,
    })
}

fn modifier_bit(token: &str) -> Option<u8> {
    match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctrl_l" | "ctrl_r" | "control_l" | "control_r" | "leftctrl"
        | "rightctrl" | "leftcontrol" | "rightcontrol" => Some(2),
        "alt" | "alt_l" | "alt_r" | "leftalt" | "rightalt" => Some(4),
        "shift" | "shift_l" | "shift_r" | "leftshift" | "rightshift" => Some(1),
        "super" | "super_l" | "super_r" | "leftsuper" | "rightsuper" | "win" | "windows"
        | "lwin" | "rwin" => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::TimeUs;

    fn key(physical: &str, text: &str, modifiers: u8) -> KeyStroke {
        KeyStroke {
            physical_key: physical.to_owned(),
            display_text: Some(text.to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers,
        }
    }
    fn release(mut key: KeyStroke, modifiers: u8) -> KeyStroke {
        key.pressed = false;
        key.modifiers = modifiers;
        key
    }

    #[test]
    fn modifier_then_combination_has_one_label_in_the_same_or_next_frame() {
        let ctrl = key("X11:37", "Ctrl", 2);
        let c = key("X11:54", "Ctrl+C", 2);
        let mut history = KeyLabelHistory::default();
        assert_eq!(
            history.update(&[ctrl.clone(), c.clone()], 0, 500).unwrap(),
            "Ctrl+C"
        );
        history.clear();
        assert_eq!(history.update(&[ctrl], 0, 500).unwrap(), "Ctrl");
        assert_eq!(history.update(&[], 100_000, 500).unwrap(), "Ctrl");
        assert_eq!(history.update(&[c], 200_000, 500).unwrap(), "Ctrl+C");
        assert_eq!(history.update(&[], 700_000, 500).unwrap(), "Ctrl+C");
        assert!(history.update(&[], 700_001, 500).unwrap().is_empty());
    }

    #[test]
    fn modifier_taps_remain_visible_and_do_not_merge_across_release_boundaries() {
        let ctrl = key("X11:37", "Ctrl", 2);
        let c = key("X11:54", "C", 2);
        let mut history = KeyLabelHistory::default();
        assert_eq!(
            history
                .update(&[ctrl.clone(), release(ctrl.clone(), 0)], 0, 500)
                .unwrap(),
            "Ctrl"
        );
        assert_eq!(
            history.update(&[ctrl, c], 100_000, 500).unwrap(),
            "Ctrl  Ctrl+C"
        );
    }

    #[test]
    fn successive_modifier_prefixes_collapse_and_left_right_variants_share_a_label() {
        let mut history = KeyLabelHistory::default();
        let events = [
            key("left", "Control_L", 2),
            key("right", "Control_R", 2),
            release(key("left", "Control_L", 2), 2),
            key("shift", "Ctrl+Shift", 3),
            key("c", "Ctrl+Shift+C", 3),
        ];
        assert_eq!(history.update(&events, 0, 500).unwrap(), "Ctrl+Shift+C");
    }

    #[test]
    fn repeated_presses_deduplicate_but_releases_and_other_keys_preserve_real_input() {
        let a = key("a", "A", 0);
        let b = key("b", "B", 0);
        let mut repeat = a.clone();
        repeat.repeat = true;
        let mut history = KeyLabelHistory::default();
        assert_eq!(
            history
                .update(
                    &[
                        a.clone(),
                        a.clone(),
                        repeat,
                        release(a.clone(), 0),
                        a.clone(),
                        b,
                        a
                    ],
                    0,
                    500
                )
                .unwrap(),
            "A  A  B  A"
        );
    }

    #[test]
    fn modifier_recognition_uses_complete_tokens_and_preserves_plus_arrows_and_unicode() {
        for (text, modifiers, expected) in [
            ("ControlCenter", 2, "Ctrl+ControlCenter"),
            ("Alternate", 4, "Alt+Alternate"),
            ("Shifter", 1, "Shift+Shifter"),
            ("Supernova", 8, "Super+Supernova"),
            ("Ctrl++", 2, "Ctrl++"),
            ("Left", 2, "Ctrl+Left"),
            ("字", 2, "Ctrl+字"),
            ("Control+CTRL+C", 2, "Ctrl+C"),
            (" ", 0, "Space"),
            ("\t", 0, "Tab"),
        ] {
            assert_eq!(
                key_label(&key("physical", text, modifiers))
                    .unwrap()
                    .identity
                    .text,
                expected
            );
        }
        assert_eq!(
            key_label(&key("X11:37", "", 2)).unwrap().identity.text,
            "Ctrl+X11:37"
        );
    }

    #[test]
    fn mapping_changes_do_not_get_mistaken_for_duplicate_keys() {
        let mut history = KeyLabelHistory::default();
        assert_eq!(
            history
                .update(
                    &[key("same-code", "A", 0), key("same-code", "Α", 0)],
                    0,
                    500
                )
                .unwrap(),
            "A  Α"
        );
    }

    #[test]
    fn unmapped_empty_keys_and_releases_do_not_invent_labels_or_require_valid_display_text() {
        let mut history = KeyLabelHistory::default();
        let unmapped = key("", "", 2);
        let invalid_release = release(key("released", "\0", 0), 0);
        let mut invalid_repeat = key("repeat", "\0", 0);
        invalid_repeat.repeat = true;
        assert!(
            history
                .update(&[unmapped, invalid_release, invalid_repeat], 0, 500)
                .unwrap()
                .is_empty()
        );
        assert_eq!(history.update(&[key("a", "A", 0)], 0, 500).unwrap(), "A");
    }

    #[test]
    fn sparse_old_events_begin_their_hold_when_visible_and_future_events_are_ignored() {
        let old = key("old", "Old", 0);
        let mut future = key("future", "Future", 0);
        future.at = TimeUs::new(20_000_000);
        let mut history = KeyLabelHistory::default();
        assert_eq!(
            history.update(&[old, future], 10_000_000, 500).unwrap(),
            "Old"
        );
        assert_eq!(history.update(&[], 10_500_000, 500).unwrap(), "Old");
        assert!(history.update(&[], 10_500_001, 500).unwrap().is_empty());
    }

    #[test]
    fn clear_and_backwards_clocks_do_not_carry_previous_labels() {
        let mut history = KeyLabelHistory::default();
        history.update(&[key("a", "A", 0)], 200_000, 500).unwrap();
        assert_eq!(
            history.update(&[key("b", "B", 0)], 100_000, 500).unwrap(),
            "B"
        );
        history.clear();
        assert!(history.update(&[], 100_000, 500).unwrap().is_empty());
    }

    #[test]
    fn label_and_event_limits_leave_previous_history_unchanged() {
        let mut history = KeyLabelHistory::default();
        history.update(&[key("a", "A", 0)], 0, 500).unwrap();
        let mut invalid = key("b", "B", 0);
        invalid.display_text = Some("x".repeat(MAX_LABEL_BYTES + 1));
        assert!(history.update(&[invalid], 100_000, 500).is_err());
        assert!(
            history
                .update(&vec![key("b", "B", 0); MAX_FRAME_EVENTS + 1], 100_000, 500)
                .is_err()
        );
        assert_eq!(history.update(&[], 100_000, 500).unwrap(), "A");
        let labels = (0..=MAX_ACTIVE_LABELS)
            .map(|index| key(&index.to_string(), &index.to_string(), 0))
            .collect::<Vec<_>>();
        assert!(KeyLabelHistory::default().update(&labels, 0, 500).is_err());
        let labels = [
            key("one", &"x".repeat(3000), 0),
            key("two", &"y".repeat(3000), 0),
        ];
        assert!(KeyLabelHistory::default().update(&labels, 0, 500).is_err());
    }
}
