//! Explicit, named positions, independent of automatic
//! playback checkpoints (port of Python `named_bookmarks.py`).
//!
//! The store lives in the configuration under
//! `named_playback_bookmarks`: a map from the resume item key
//! (64 lowercase hex) to rows of `{id, name, position_ms}`,
//! sorted by position then id, capped at 1000 rows in total.
//! The JSON shape is Python's, so both programs read each
//! other's bookmarks.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::bookmarks::MAX_MS;

/// Python `LIMIT`: the most bookmarks kept across all items.
pub const LIMIT: usize = 1000;

/// One bookmark row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedBookmark {
    pub id: String,
    pub name: String,
    pub position_ms: i64,
}

fn is_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_identity(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Python `clean_name`: a trimmed name of 1 to 120 characters
/// with no control characters.
///
/// # Errors
/// Python's message for anything else.
pub fn clean_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(|c| (c as u32) < 32) {
        return Err("Enter a bookmark name of 1 to 120 characters.".to_string());
    }
    Ok(name.to_string())
}

fn row_value(row: &NamedBookmark) -> Value {
    serde_json::json!({
        "id": row.id,
        "name": row.name,
        "position_ms": row.position_ms,
    })
}

/// The store as a configuration value (Python's dict shape).
#[must_use]
pub fn to_value(store: &BTreeMap<String, Vec<NamedBookmark>>) -> Value {
    let mut object = Map::new();
    for (key, rows) in store {
        object.insert(
            key.clone(),
            Value::Array(rows.iter().map(row_value).collect()),
        );
    }
    Value::Object(object)
}

/// Python `normalize`: the validated store — bad keys, rows
/// and duplicates dropped, rows sorted, the global cap
/// applied in iteration order.
#[must_use]
pub fn normalize(value: &Value) -> BTreeMap<String, Vec<NamedBookmark>> {
    let mut result: BTreeMap<String, Vec<NamedBookmark>> = BTreeMap::new();
    let Some(object) = value.as_object() else {
        return result;
    };
    let mut count = 0usize;
    for (key, entries) in object {
        if !is_key(key) {
            continue;
        }
        let Some(entries) = entries.as_array() else {
            continue;
        };
        let mut rows: Vec<NamedBookmark> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for entry in entries {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let identity = entry.get("id").and_then(Value::as_str).unwrap_or("");
            let position = entry.get("position_ms").and_then(Value::as_i64);
            let Some(position) = position else {
                continue;
            };
            if !is_identity(identity) || seen.iter().any(|id| id == identity) {
                continue;
            }
            if !(0..=MAX_MS).contains(&position) {
                continue;
            }
            let Ok(name) = clean_name(entry.get("name").and_then(Value::as_str).unwrap_or(""))
            else {
                continue;
            };
            seen.push(identity.to_string());
            rows.push(NamedBookmark {
                id: identity.to_string(),
                name,
                position_ms: position,
            });
            count += 1;
            if count >= LIMIT {
                break;
            }
        }
        if !rows.is_empty() {
            rows.sort_by(|a, b| {
                a.position_ms
                    .cmp(&b.position_ms)
                    .then_with(|| a.id.cmp(&b.id))
            });
            result.insert(key.clone(), rows);
        }
        if count >= LIMIT {
            break;
        }
    }
    result
}

/// The rows for one item key, normalized.
#[must_use]
pub fn rows_for(value: &Value, key: &str) -> Vec<NamedBookmark> {
    normalize(value).get(key).cloned().unwrap_or_default()
}

fn new_identity() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // No entropy source: fall back to the time and a
        // process counter; the id only needs to be unique.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let mixed = now ^ COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
        bytes[..8].copy_from_slice(&mixed.to_be_bytes());
        bytes[8..].copy_from_slice(&(mixed.wrapping_mul(0x2545_F491_4F6C_DD1D)).to_be_bytes());
    }
    bytes.iter().fold(String::with_capacity(32), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Python `put`: add a bookmark, or rename the one with the
/// given identity. Returns the whole normalized store.
///
/// # Errors
/// Python's messages: an unusable key reports as the
/// on-demand availability error, a bad name, an unavailable
/// position, a vanished identity, a duplicate name
/// (case-insensitive) or the reached limit.
pub fn put(
    value: &Value,
    key: &str,
    name: &str,
    position: i64,
    identity: Option<&str>,
) -> Result<Value, String> {
    let mut store = normalize(value);
    if !is_key(key) {
        return Err("This command is available during on-demand playback.".to_string());
    }
    let name = clean_name(name)?;
    if !(0..=MAX_MS).contains(&position) {
        return Err("The playback position is not available yet.".to_string());
    }
    let rows = store.get(key).cloned().unwrap_or_default();
    if let Some(identity) = identity {
        if !rows.iter().any(|row| row.id == identity) {
            return Err("The bookmark is no longer available. Reopen the list.".to_string());
        }
    }
    let folded: String = name.chars().flat_map(char::to_lowercase).collect();
    if rows.iter().any(|row| {
        row.id != identity.unwrap_or("")
            && row
                .name
                .chars()
                .flat_map(char::to_lowercase)
                .collect::<String>()
                == folded
    }) {
        return Err("A bookmark with this name already exists for this item.".to_string());
    }
    if identity.is_none() && store.values().map(Vec::len).sum::<usize>() >= LIMIT {
        return Err(
            "The bookmark limit has been reached. Delete an unused bookmark first.".to_string(),
        );
    }
    let identity = identity.map_or_else(new_identity, str::to_string);
    let mut rows: Vec<NamedBookmark> = rows.into_iter().filter(|row| row.id != identity).collect();
    rows.push(NamedBookmark {
        id: identity,
        name,
        position_ms: position,
    });
    store.insert(key.to_string(), rows);
    Ok(to_value(&normalize(&to_value(&store))))
}

/// Python `remove`: drop one bookmark, returning the whole
/// normalized store.
#[must_use]
pub fn remove(value: &Value, key: &str, identity: &str) -> Value {
    let mut store = normalize(value);
    if let Some(rows) = store.get_mut(key) {
        rows.retain(|row| row.id != identity);
    }
    to_value(&normalize(&to_value(&store)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn store_with(rows: &str) -> Value {
        serde_json::json!({ KEY: serde_json::from_str::<Value>(rows).unwrap() })
    }

    #[test]
    fn clean_name_rules_match_python() {
        assert_eq!(clean_name("  Intro  "), Ok("Intro".to_string()));
        assert!(clean_name("").is_err());
        assert!(clean_name("   ").is_err());
        assert!(clean_name(&"x".repeat(121)).is_err());
        assert!(clean_name("a\u{1}b").is_err());
        assert_eq!(
            clean_name(""),
            Err("Enter a bookmark name of 1 to 120 characters.".to_string())
        );
    }

    #[test]
    fn normalize_drops_bad_rows_and_sorts() {
        let value = store_with(
            r#"[
              {"id": "0123456789abcdef0123456789abcdef", "name": "Late", "position_ms": 9000},
              {"id": "0123456789abcdef0123456789abcdeg", "name": "Bad id", "position_ms": 1},
              {"id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "name": "Early", "position_ms": 100},
              {"id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "name": "Dup id", "position_ms": 200},
              {"id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "name": "Too far", "position_ms": 999999999999},
              {"id": "cccccccccccccccccccccccccccccccc", "name": "", "position_ms": 5}
            ]"#,
        );
        let rows = rows_for(&value, KEY);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Early");
        assert_eq!(rows[1].name, "Late");
        // A non-map store and a bad key both normalize away.
        assert!(rows_for(&Value::Null, KEY).is_empty());
        assert!(rows_for(&serde_json::json!({"zz": []}), KEY).is_empty());
    }

    #[test]
    fn put_adds_renames_and_guards() {
        let empty = Value::Null;
        let added = put(&empty, KEY, "Start", 500, None).unwrap();
        let rows = rows_for(&added, KEY);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Start");
        assert!(is_identity(&rows[0].id));
        // Duplicate name, case-insensitive.
        assert_eq!(
            put(&added, KEY, "start", 600, None),
            Err("A bookmark with this name already exists for this item.".to_string())
        );
        // Rename through the identity keeps the position.
        let renamed = put(&added, KEY, "Opening", 500, Some(&rows[0].id)).unwrap();
        assert_eq!(rows_for(&renamed, KEY)[0].name, "Opening");
        // A vanished identity and a bad key report Python's lines.
        assert_eq!(
            put(&added, KEY, "X", 1, Some(ID)),
            Err("The bookmark is no longer available. Reopen the list.".to_string())
        );
        assert_eq!(
            put(&added, "nope", "X", 1, None),
            Err("This command is available during on-demand playback.".to_string())
        );
        assert_eq!(
            put(&added, KEY, "X", MAX_MS + 1, None),
            Err("The playback position is not available yet.".to_string())
        );
    }

    #[test]
    fn remove_drops_only_the_named_row() {
        let value = store_with(
            r#"[
              {"id": "0123456789abcdef0123456789abcdef", "name": "One", "position_ms": 1},
              {"id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "name": "Two", "position_ms": 2}
            ]"#,
        );
        let after = remove(&value, KEY, ID);
        let rows = rows_for(&after, KEY);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Two");
    }
}
