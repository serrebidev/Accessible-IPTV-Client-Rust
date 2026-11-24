//! Provider-scoped channel preferences shared by the guide and the
//! player, ported from Python `channel_preferences.py`: the user's
//! guide assignment per channel (`epg_channel_overrides`, keyed by the
//! reminder channel key) and the per-channel volume memory
//! (`channel_volumes`, same key). Like Python's, the assignment
//! table is process-wide so every guide lookup, on any thread,
//! honours it.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_json::{Map, Value};

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

/// Python's cap on the remembered-volume map: the newest 5000 entries.
pub const MAX_CHANNEL_VOLUMES: usize = 5000;

/// The remembered volume for `key` (Python `volume`): the stored
/// value only when it is an integer in 0..=200, otherwise `default`.
#[must_use]
pub fn channel_volume(config: &Value, key: &str, default: i64) -> i64 {
    let value = config
        .get("channel_volumes")
        .and_then(Value::as_object)
        .and_then(|values| values.get(key));
    match value {
        Some(value) => value
            .as_i64()
            .filter(|v| (0..=200).contains(v))
            .unwrap_or(default),
        None => default,
    }
}

/// Remember one volume (Python `remember_volume`): the value is
/// clamped to 0..=200, re-inserted as the newest entry, and the map
/// is trimmed to the newest [`MAX_CHANNEL_VOLUMES`] entries. (The
/// JSON map is key-ordered rather than insertion-ordered, so the
/// trim evicts by key — the same documented divergence as the
/// per-channel audio-track memory in `audiotracks`.)
pub fn remember_volume(values: &mut Map<String, Value>, key: &str, value: i64) {
    values.remove(key);
    values.insert(key.to_string(), Value::from(value.clamp(0, 200)));
    while values.len() > MAX_CHANNEL_VOLUMES {
        let Some(oldest) = values.keys().next().cloned() else {
            break;
        };
        values.remove(&oldest);
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

    #[test]
    fn volume_reads_only_valid_stored_integers() {
        let cfg = serde_json::json!({"channel_volumes": {"k": 42}});
        assert_eq!(channel_volume(&cfg, "k", 100), 42);
        assert_eq!(channel_volume(&cfg, "missing", 100), 100);
        for bad in [
            serde_json::json!({"channel_volumes": {"k": 201}}),
            serde_json::json!({"channel_volumes": {"k": -1}}),
            serde_json::json!({"channel_volumes": {"k": 50.5}}),
            serde_json::json!({"channel_volumes": {"k": "50"}}),
            serde_json::json!({"channel_volumes": {"k": true}}),
            serde_json::json!({"channel_volumes": [1, 2]}),
            serde_json::json!({}),
        ] {
            assert_eq!(channel_volume(&bad, "k", 100), 100, "{bad}");
        }
        // Bounds are valid values, not defaults.
        let cfg = serde_json::json!({"channel_volumes": {"lo": 0, "hi": 200}});
        assert_eq!(channel_volume(&cfg, "lo", 100), 0);
        assert_eq!(channel_volume(&cfg, "hi", 100), 200);
    }

    #[test]
    fn remember_clamps_and_replaces() {
        let mut values = Map::new();
        remember_volume(&mut values, "k", 250);
        assert_eq!(values["k"], serde_json::json!(200));
        remember_volume(&mut values, "k", -5);
        assert_eq!(values["k"], serde_json::json!(0));
        assert_eq!(values.len(), 1);
        remember_volume(&mut values, "k", 75);
        assert_eq!(
            channel_volume(&serde_json::json!({"channel_volumes": values}), "k", 100),
            75
        );
    }

    #[test]
    fn remember_trims_to_the_cap() {
        let mut values = Map::new();
        for i in 0..=MAX_CHANNEL_VOLUMES {
            remember_volume(&mut values, &format!("k{i:05}"), 10);
        }
        assert_eq!(values.len(), MAX_CHANNEL_VOLUMES);
    }
}
