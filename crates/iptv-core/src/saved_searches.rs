//! Saved programme searches, ported from Python `saved_searches.py`.
//! This round holds the alert history (`programme-search-state.json`),
//! which full backups carry; the rules and scans join it with C11.

use serde_json::{Map, Value};

/// Python `MAX_SEEN`.
pub const MAX_SEEN: usize = 20_000;

const DAMAGED: &str = "The saved search history is damaged.";

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Python `normalize_seen`: matched programme ids (64 hex) with their
/// end times, kept only while the end is in the next 16 days, oldest
/// end first, at most [`MAX_SEEN`].
///
/// # Errors
/// Returns Python's message when the value is not an object.
pub fn normalize_seen(value: &Value, now: f64) -> Result<Value, String> {
    let rows = value.as_object().ok_or_else(|| DAMAGED.to_string())?;
    let mut kept: Vec<(&String, f64, &Value)> = rows
        .iter()
        .filter(|(key, _)| is_hex64(key))
        .filter_map(|(key, end)| {
            let at = end.as_f64().filter(|at| at.is_finite())?;
            (now < at && at <= now + 16.0 * 86_400.0).then_some((key, at, end))
        })
        .collect();
    kept.sort_by(|a, b| a.1.total_cmp(&b.1));
    kept.truncate(MAX_SEEN);
    let map: Map<String, Value> = kept
        .into_iter()
        .map(|(key, _, end)| (key.clone(), end.clone()))
        .collect();
    Ok(Value::Object(map))
}

/// Python `read_seen`: the history file, empty when absent.
///
/// # Errors
/// Returns Python's message for a file over 4 MiB, unreadable JSON or
/// a non-object.
pub fn read_seen(path: &std::path::Path, now: f64) -> Result<Value, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Map::new()));
        }
        Err(error) => return Err(error.to_string()),
    };
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(DAMAGED.to_string());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| DAMAGED.to_string())?;
    normalize_seen(&value, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_keeps_only_upcoming_ids() {
        let now = 1_000_000.0;
        let id = |c: char| c.to_string().repeat(64);
        let value = serde_json::json!({
            id('a'): now + 60.0,
            id('b'): now - 1.0,
            id('c'): now + 17.0 * 86_400.0,
            "short": now + 60.0,
            id('d'): "later",
        });
        let kept = normalize_seen(&value, now).unwrap();
        assert_eq!(kept, serde_json::json!({ id('a'): now + 60.0 }));
        assert!(normalize_seen(&serde_json::json!([]), now).is_err());
    }
}
