//! Parsing for `commands/` topics.

/// Parsed identity of a commands/ topic: which device, and which command
/// (verb + target — matched against each `x-command-source` entry's own
/// `verb`/`target` fields, joined `{verb}_{target}` to key the lookup map).
pub struct CommandTopic<'t> {
    /// The device the command targets.
    pub device_id: &'t str,
    /// The command verb (e.g. `set`, `enable`).
    pub verb: &'t str,
    /// The command target within the device (e.g. `active_power`).
    pub target: &'t str,
}

/// Parse a commands/ topic into device id + verb + target, scoped to our site.
///
/// Topic shape (system_adr §9):
/// `sites/{site}/devices/{dev}/commands/{verb}/{target}/{unit}`.
pub fn parse_command_topic<'t>(topic: &'t str, site_id: &str) -> Option<CommandTopic<'t>> {
    let mut parts = topic.split('/');
    if parts.next() != Some("sites")
        || parts.next() != Some(site_id)
        || parts.next() != Some("devices")
    {
        return None;
    }
    let device_id = parts.next().filter(|s| !s.is_empty())?;
    if parts.next() != Some("commands") {
        return None;
    }
    let verb = parts.next().filter(|s| !s.is_empty())?;
    let target = parts.next().filter(|s| !s.is_empty())?;
    Some(CommandTopic {
        device_id,
        verb,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_verb_target_from_command_topic() {
        // Arrange
        let topic = "sites/s1/devices/bess_module_01/commands/set/active_power/watts";
        // Act
        let parsed = parse_command_topic(topic, "s1").unwrap();
        // Assert
        assert_eq!(parsed.device_id, "bess_module_01");
        assert_eq!(parsed.verb, "set");
        assert_eq!(parsed.target, "active_power");
    }

    #[test]
    fn rejects_other_site_and_non_command_topics() {
        // Arrange + Act + Assert — wrong site
        assert!(parse_command_topic("sites/other/devices/d/commands/set/x/w", "s1").is_none());
        // measurements family is not a command
        assert!(parse_command_topic("sites/s1/devices/d/measurements/x/w", "s1").is_none());
        // truncated topic — missing device
        assert!(parse_command_topic("sites/s1/devices", "s1").is_none());
        // truncated topic — missing target
        assert!(parse_command_topic("sites/s1/devices/d/commands/set", "s1").is_none());
    }
}
