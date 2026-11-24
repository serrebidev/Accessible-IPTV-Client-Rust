//! Provider-scoped channel preferences shared by the guide and the
//! player, ported from Python `channel_preferences.py`: the user's
//! guide assignment per channel (`epg_channel_overrides`, keyed by the
//! reminder channel key). Like Python's, the table is process-wide so
//! every guide lookup, on any thread, honours it.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_json::Value;

use crate::Channel;

static OVERRIDES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// The config key Python stores assignments under.
pub const OVERRIDES_KEY: &str = "epg_channel_overrides";

/// Python `configure`: load the assignments from the config (64-hex
/// channel keys to guide ids of 1 to 1024 characters).
pub fn configure(config: &Value) {
    let table: HashMap<String, String> = config
        .get(OVERRIDES_KEY)
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    let id = value.as_str()?;
                    (key.len() == 64 && !id.is_empty() && id.chars().count() <= 1024)
                        .then(|| (key.clone(), id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Ok(mut slot) = OVERRIDES.write() {
        *slot = Some(table);
    }
}

/// Python `epg_id`: the guide id the user assigned to `channel`, or
/// empty for automatic matching.
#[must_use]
pub fn epg_id(channel: &Channel) -> String {
    let Ok(slot) = OVERRIDES.read() else {
        return String::new();
    };
    match slot.as_ref() {
        Some(table) if !table.is_empty() => table
            .get(&crate::reminders::reminder_channel_key(channel))
            .cloned()
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The config value after assigning `guide_id` to `channel` (empty
/// removes the assignment, back to automatic matching).
#[must_use]
pub fn with_assignment(config: &Value, channel: &Channel, guide_id: &str) -> Value {
    let mut values = config
        .get(OVERRIDES_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let key = crate::reminders::reminder_channel_key(channel);
    if guide_id.is_empty() {
        values.remove(&key);
    } else {
        values.insert(key, Value::String(guide_id.to_string()));
    }
    Value::Object(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process-wide table: one test owns it and resets it.
    #[test]
    fn an_assignment_wins_and_a_vanished_one_stays_unavailable() {
        let channel = Channel {
            name: "One News".to_string(),
            tvg_id: "one.news".to_string(),
            url: "http://example.com/one".to_string(),
            ..Channel::default()
        };
        let indexes = crate::epg::GuideIndexes::build(&[
            crate::epg::EpgChannel {
                id: "one.news".to_string(),
                display_name: "One News".to_string(),
            },
            crate::epg::EpgChannel {
                id: "other.id".to_string(),
                display_name: "Other".to_string(),
            },
        ]);
        let find = || crate::epg::match_guide_channel(&channel, &indexes, Some(&indexes.known_ids));
        assert_eq!(find().as_deref(), Some("one.news"));
        let config = serde_json::json!({ OVERRIDES_KEY: {} });
        let config =
            serde_json::json!({ OVERRIDES_KEY: with_assignment(&config, &channel, "other.id") });
        configure(&config);
        assert_eq!(epg_id(&channel), "other.id");
        assert_eq!(find().as_deref(), Some("other.id"));
        let gone =
            serde_json::json!({ OVERRIDES_KEY: with_assignment(&config, &channel, "removed.id") });
        configure(&gone);
        assert_eq!(find(), None);
        let automatic = serde_json::json!({ OVERRIDES_KEY: with_assignment(&gone, &channel, "") });
        assert_eq!(automatic[OVERRIDES_KEY], serde_json::json!({}));
        configure(&automatic);
        assert_eq!(find().as_deref(), Some("one.news"));
    }
}
