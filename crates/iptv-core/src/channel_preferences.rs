//! Per-channel volume memory (port of the volume half of Python's
//! `channel_preferences.py`; the guide-override half of that module
//! lands with the Assign Programme Guide port in `guide_match`).
//!
//! Volumes live in the config's `channel_volumes` map, keyed by the
//! reminder-scope channel key (`reminders::reminder_channel_key`:
//! sha256 of the provider/playlist scope plus the favorites identity),
//! so a value Python wrote is the value Rust reads and the reverse.

use serde_json::{Map, Value};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(raw: serde_json::Value) -> Value {
        raw
    }

    #[test]
    fn volume_reads_only_valid_stored_integers() {
        let cfg = config(serde_json::json!({"channel_volumes": {"k": 42}}));
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
        let cfg = config(serde_json::json!({"channel_volumes": {"lo": 0, "hi": 200}}));
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
