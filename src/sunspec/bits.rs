//! Bitfield command words (EPC CmdBits): the word the gateway asserts per
//! device, rewritten by the heartbeat and changed one bit per command.
//!
//! The bits the gateway has bindings for start at 0 (En = 0: stopped until
//! the EMS enables it); the rest keep what the device read at seeding. The
//! word is forgotten whenever the heartbeat stops (broker lost, device
//! locked out), so a PCS that tripped never resumes on its own.

use crate::asyncapi::types::SunspecBinding;
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// A command word's device (host, port, unit id) and point (model, id).
pub type WordKey = (String, u16, String, u16, String);

/// The asserted word, `None` until the heartbeat seeds it.
pub type Slot = Arc<AsyncMutex<Option<u32>>>;

/// Reason: one async lock per word, held across its write, so a beat can't
/// land a stale word between a command's write and its store; per word so
/// a dead PCS's retries never delay another's beat.
static WORDS: LazyLock<Mutex<HashMap<WordKey, Slot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// What a bit command does to its bit.
#[derive(Debug, PartialEq, Eq)]
pub enum BitAction {
    /// Latch the bit on.
    Set,
    /// Latch the bit off.
    Clear,
    /// A trigger: written on once, cleared by the next beat.
    Pulse,
}

/// The action a `verb` (with the frame's `value`) asks of a bit.
pub fn action(verb: &str, value: f64) -> Result<BitAction> {
    Ok(match verb {
        "enable" | "start" => BitAction::Set,
        "disable" | "stop" => BitAction::Clear,
        "clear" | "reset" => BitAction::Pulse,
        "set" if value != 0.0 => BitAction::Set,
        "set" => BitAction::Clear,
        other => bail!("verb {other} doesn't drive a bit"),
    })
}

/// The word `b`'s bit lives in.
pub fn word_key(b: &SunspecBinding) -> WordKey {
    (
        b.host.clone(),
        b.port,
        b.unit_id.clone(),
        b.model_id,
        b.point.clone(),
    )
}

/// The slot for `key`, created empty on first use.
pub fn slot(key: &WordKey) -> Slot {
    WORDS
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone()
}

/// The word to write for `action` on `bit` of `word`, and the word to keep
/// asserting after it.
pub fn apply(word: Option<u32>, bit: u8, action: &BitAction) -> Result<(u32, u32)> {
    let word = word.ok_or_else(|| anyhow!("no heartbeat running; the PCS isn't listening"))?;
    let mask = 1u32 << bit;
    Ok(match action {
        BitAction::Set => (word | mask, word | mask),
        BitAction::Clear => (word & !mask, word & !mask),
        BitAction::Pulse => (word | mask, word & !mask),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_and_disable_latch_their_bit() {
        assert_eq!(
            apply(Some(0b100), 0, &BitAction::Set).unwrap(),
            (0b101, 0b101)
        );
        assert_eq!(
            apply(Some(0b101), 0, &BitAction::Clear).unwrap(),
            (0b100, 0b100)
        );
    }

    #[test]
    fn a_trigger_is_written_once_and_not_kept() {
        assert_eq!(
            apply(Some(0b001), 1, &BitAction::Pulse).unwrap(),
            (0b011, 0b001)
        );
    }

    #[test]
    fn a_bit_command_without_a_heartbeat_is_refused() {
        assert!(apply(None, 0, &BitAction::Set).is_err());
    }

    #[test]
    fn verbs_map_to_actions() {
        assert_eq!(action("enable", 0.0).unwrap(), BitAction::Set);
        assert_eq!(action("disable", 1.0).unwrap(), BitAction::Clear);
        assert_eq!(action("clear", 1.0).unwrap(), BitAction::Pulse);
        assert_eq!(action("set", 0.0).unwrap(), BitAction::Clear);
        assert!(action("ramp", 1.0).is_err());
    }
}
