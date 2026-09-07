//! XKB groups and real modifier maps, not assumed US keycodes/Mod1/Mod4.

use super::super::{ShortcutKey, ShortcutTrigger};
use super::{Client, DEVICE, Grab, ShortcutBinding};
use std::collections::BTreeSet;
use x11rb::{
    connection::Connection,
    protocol::{
        xkb::{self, ConnectionExt as _, KeySymMap},
        xproto::{ConnectionExt as _, ModMask},
    },
};

pub(super) struct MappingSnapshot {
    first: u8,
    symbols: Vec<KeySymMap>,
    modifier_masks: [u16; 256],
    pub(super) group: u8,
}

impl MappingSnapshot {
    pub(super) fn read(connection: &Client<'_>) -> Result<Self, String> {
        let map = connection
            .xkb_get_map(
                DEVICE,
                xkb::MapPart::KEY_SYMS,
                xkb::MapPart::default(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                xkb::VMod::default(),
                0,
                0,
                0,
                0,
                0,
                0,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let symbols = map.map.syms_rtrn.ok_or("XKB did not provide key symbols")?;
        if symbols.len() > 256
            || symbols.len() != usize::from(map.n_key_syms)
            || usize::from(map.first_key_sym) + symbols.len() > 256
            || symbols
                .iter()
                .any(|key| key.syms.len() > 1024 || key.group_info & 15 > 4)
        {
            return Err("Invalid or excessive XKB key map".into());
        }
        let mods = connection
            .get_modifier_mapping()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if mods.keycodes.len() > 2048 || !mods.keycodes.len().is_multiple_of(8) {
            return Err("Invalid X11 modifier map".into());
        }
        let mut modifier_masks = [0_u16; 256];
        let columns = mods.keycodes.len() / 8;
        if columns > 0 {
            for (index, codes) in mods.keycodes.chunks(columns).enumerate() {
                for code in codes {
                    if *code != 0 {
                        modifier_masks[usize::from(*code)] |= 1 << index;
                    }
                }
            }
        }
        let state = connection
            .xkb_get_state(DEVICE)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let group = u8::from(state.group);
        if group > 3 {
            return Err("Invalid active XKB group".into());
        }
        let setup = connection.setup();
        if map.first_key_sym < setup.min_keycode
            || usize::from(map.first_key_sym) + symbols.len() > usize::from(setup.max_keycode) + 1
        {
            return Err("XKB key symbols fall outside the server's keycode range".into());
        }
        Ok(Self {
            first: map.first_key_sym,
            symbols,
            modifier_masks,
            group,
        })
    }

    fn symbol(&self, code: u8) -> Option<u32> {
        let key = self
            .symbols
            .get(usize::from(code.checked_sub(self.first)?))?;
        base_symbol(key, self.group)
    }

    fn modifiers(&self, names: &[u32]) -> BTreeSet<u16> {
        self.modifier_masks
            .iter()
            .enumerate()
            .filter_map(|(code, mask)| {
                let code = u8::try_from(code).ok()?;
                (*mask != 0
                    && self
                        .symbol(code)
                        .is_some_and(|symbol| names.contains(&symbol)))
                .then_some(*mask)
            })
            .collect()
    }

    fn ignored_locks(&self) -> Result<u16, String> {
        let ignored = self
            .modifiers(&[0xffe5, 0xffe6, 0xff7f])
            .into_iter()
            .fold(u16::from(ModMask::LOCK), |a, b| a | b);
        if ignored.count_ones() > 3 {
            return Err("Caps/Num lock mapping has too many independent modifier bits".into());
        }
        Ok(ignored)
    }

    fn trigger_modifiers(&self, trigger: ShortcutTrigger, locks: u16) -> Result<Vec<u16>, String> {
        let mut masks = vec![0_u16];
        for (enabled, names, label) in [
            (trigger.control, &[0xffe3, 0xffe4][..], "Control"),
            (trigger.alt, &[0xffe9, 0xffea][..], "Alt"),
            (trigger.shift, &[0xffe1, 0xffe2][..], "Shift"),
            (trigger.super_key, &[0xffeb, 0xffec][..], "Super"),
        ] {
            if !enabled {
                continue;
            }
            let mut choices = self.modifiers(names);
            if choices.is_empty() && label == "Super" {
                choices = self.modifiers(&[0xffe7, 0xffe8]);
            }
            if choices.is_empty() {
                return Err(format!("{label} is not mapped as an X11 modifier"));
            }
            let mut next = BTreeSet::new();
            for old in &masks {
                for choice in &choices {
                    if choice & locks != 0 || old & choice != 0 {
                        return Err("Requested modifiers overlap each other or Caps/Num lock; refusing an ambiguous shortcut".into());
                    }
                    next.insert(old | choice);
                }
            }
            if next.len() > 16 {
                return Err("Shortcut modifier combinations exceed the safe limit".into());
            }
            masks = next.into_iter().collect();
        }
        Ok(masks)
    }

    pub(super) fn resolve(&self, bindings: &[ShortcutBinding]) -> Result<Vec<Grab>, String> {
        let locks = self.ignored_locks()?;
        let mut output: Vec<Grab> = Vec::new();
        for binding in bindings {
            let codes = self
                .symbols
                .iter()
                .enumerate()
                .filter_map(|(index, key)| {
                    let code = u8::try_from(usize::from(self.first) + index).ok()?;
                    (code >= 8
                        && base_symbol(key, self.group)
                            .is_some_and(|symbol| matches_key(symbol, binding.trigger.key)))
                    .then_some(code)
                })
                .collect::<Vec<_>>();
            if codes.is_empty() {
                return Err(format!(
                    "{} is unavailable in the active XKB base layout",
                    binding.trigger.label()
                ));
            }
            if codes.len() > 8 {
                return Err(
                    "A shortcut maps to more than eight physical keys; refusing broad registration"
                        .into(),
                );
            }
            let masks = self.trigger_modifiers(binding.trigger, locks)?;
            for code in codes {
                for mask in &masks {
                    for lock_state in lock_combinations(locks) {
                        let modifiers = mask | lock_state;
                        if let Some(existing) = output
                            .iter()
                            .find(|g| g.code == code && g.modifiers == modifiers)
                        {
                            if existing.action != binding.action {
                                return Err("Two recorder actions resolve to the same physical X11 shortcut".into());
                            }
                        } else {
                            output.push(Grab {
                                code,
                                modifiers,
                                action: binding.action,
                            });
                        }
                    }
                }
            }
        }
        Ok(output)
    }
}

fn lock_combinations(mask: u16) -> impl Iterator<Item = u16> {
    (0_u16..=255).filter(move |value| value & !mask == 0)
}

fn matches_key(symbol: u32, key: ShortcutKey) -> bool {
    match key {
        ShortcutKey::Function(number) => symbol == 0xffbe + u32::from(number) - 1,
        ShortcutKey::Character(letter) => {
            let symbol = if (u32::from(b'A')..=u32::from(b'Z')).contains(&symbol) {
                symbol + 32
            } else {
                symbol
            };
            symbol == u32::from(letter.to_ascii_lowercase())
        }
    }
}

fn base_symbol(key: &KeySymMap, current: u8) -> Option<u32> {
    let groups = key.group_info & 15;
    if groups == 0 || groups > 4 || key.width == 0 {
        return None;
    }
    let group = if current < groups {
        current
    } else {
        match key.group_info & 0xc0 {
            0x40 => groups - 1,
            0x80 => {
                let redirect = (key.group_info >> 4) & 3;
                if redirect < groups { redirect } else { 0 }
            }
            _ => current % groups,
        }
    };
    // XDG named triggers refer to the base/depressed layer; Shift is a
    // separate modifier, not a request to look up the shifted key symbol.
    key.syms
        .get(usize::from(group) * usize::from(key.width))
        .copied()
        .filter(|symbol| *symbol != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caps_and_num_are_explicit_combinations_not_any_modifier() {
        assert_eq!(
            lock_combinations(2 | 16).collect::<Vec<_>>(),
            [0, 2, 16, 18]
        );
    }
    #[test]
    fn named_keys_match_base_layer_without_assuming_us_codes() {
        assert!(matches_key(u32::from(b'r'), ShortcutKey::Character('R')));
        assert!(matches_key(0xffc4, ShortcutKey::Function(7)));
        let key = KeySymMap {
            group_info: 2,
            width: 2,
            syms: vec![
                u32::from(b'a'),
                u32::from(b'A'),
                u32::from(b'r'),
                u32::from(b'R'),
            ],
            ..KeySymMap::default()
        };
        assert_eq!(base_symbol(&key, 1), Some(u32::from(b'r')));
        assert_eq!(base_symbol(&key, 2), Some(u32::from(b'a')));
        assert_eq!(
            base_symbol(
                &KeySymMap {
                    group_info: 0x42,
                    ..key
                },
                3
            ),
            Some(u32::from(b'r'))
        );
    }

    #[test]
    fn alt_super_and_lock_masks_are_resolved_from_the_actual_modifier_map() {
        let mut snapshot = MappingSnapshot {
            first: 8,
            symbols: vec![KeySymMap::default(); 64],
            modifier_masks: [0; 256],
            group: 0,
        };
        for (code, symbol, mask) in [
            (30, u32::from(b'r'), 0),
            (40, 0xffe9, 32),
            (41, 0xffeb, 8),
            (42, 0xffe5, 2),
            (43, 0xff7f, 128),
        ] {
            snapshot.symbols[code - 8] = KeySymMap {
                group_info: 1,
                width: 1,
                syms: vec![symbol],
                ..KeySymMap::default()
            };
            snapshot.modifier_masks[code] = mask;
        }
        let binding = ShortcutBinding {
            action: super::super::ShortcutAction::StartPause,
            trigger: ShortcutTrigger {
                key: ShortcutKey::Character('R'),
                control: false,
                alt: true,
                shift: false,
                super_key: true,
            },
        };
        let grabs = snapshot.resolve(std::slice::from_ref(&binding)).unwrap();
        assert_eq!(
            grabs.iter().map(|grab| grab.modifiers).collect::<Vec<_>>(),
            [40, 42, 168, 170]
        );
        assert!(grabs.iter().all(|grab| grab.code == 30));
        snapshot.modifier_masks[43] = 32;
        assert!(
            snapshot
                .resolve(&[binding])
                .unwrap_err()
                .contains("overlap")
        );
    }
}
