//! Python `playback_bookmarks` (P21): a line-for-line port of
//! `playback_resume.py`. Bounded VOD progress with conservative
//! identities and credential-free locators — never matched by
//! title. Xtream IDs are scoped to the configured account,
//! content kind and (for episodes) series; unstructured
//! playlists retain exact-URL identity; legacy URL hashes
//! migrate only when that exact item is encountered again.
//!
//! Keys are SHA-256 digests of Python `json.dumps` output
//! (`ensure_ascii=False`, default separators), reproduced
//! byte-exactly so records written by the Python client are
//! consumed unchanged, and vice versa.

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::Channel;

/// Seven days in milliseconds (Python `MAX_MS`).
pub const MAX_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// A position under 30 s is not worth keeping.
pub const MIN_POSITION_MS: i64 = 30_000;
/// The store keeps at most this many records.
pub const MAX_ENTRIES: usize = 200;

/// The validated Xtream locator stored in a record's metadata:
/// account-scoped provider identity with no URL and no
/// credentials, so a bookmark survives stream-URL renewal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookmarkLocator {
    pub source: String,
    /// `"movie"` or `"episode"`.
    pub kind: String,
    pub id: String,
    /// The series id, for episodes only.
    pub series: Option<String>,
}

/// One normalized `playback_bookmarks` record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bookmark {
    pub position_ms: i64,
    pub length_ms: i64,
    pub updated: i64,
    /// The cleaned display title (empty when none survived
    /// cleaning); identification never uses it.
    pub title: String,
    pub locator: Option<BookmarkLocator>,
}

/// The whole store, by 64-char hex key.
pub type Bookmarks = BTreeMap<String, Bookmark>;

fn attr<'a>(channel: &'a Channel, key: &str) -> &'a str {
    channel
        .unknown_attributes
        .get(key)
        .map_or("", String::as_str)
}

/// Whether the channel is a playable movie item (Python
/// `kind == "movie"`; resolved Xtream episodes are movie
/// entries too). The app's own URL classification decides, as
/// it does for the resume gate in the player.
fn is_movie(channel: &Channel) -> bool {
    !crate::playback::PlaybackRequest::for_channel(channel, None).live
}

/// A Python `json.dumps` string literal with
/// `ensure_ascii=False`: only the mandatory escapes, non-ASCII
/// text passes through as UTF-8.
fn py_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// `json.dumps` of a list of strings (default separators).
fn py_json_list(items: &[&str]) -> String {
    let parts: Vec<String> = items.iter().map(|item| py_json_string(item)).collect();
    format!("[{}]", parts.join(", "))
}

/// `json.dumps` of the locator object, keys in Python's
/// insertion order: source, type, id, then series.
fn py_json_locator(locator: &BookmarkLocator) -> String {
    let mut parts = vec![
        format!("\"source\": {}", py_json_string(&locator.source)),
        format!("\"type\": {}", py_json_string(&locator.kind)),
        format!("\"id\": {}", py_json_string(&locator.id)),
    ];
    if let Some(series) = &locator.series {
        parts.push(format!("\"series\": {}", py_json_string(series)));
    }
    format!("{{{}}}", parts.join(", "))
}

fn digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    crate::hex_lower(&hasher.finalize())
}

/// Python `_identifier`: `[1-9][0-9]{0,19}` or empty.
fn identifier(value: &str) -> String {
    let valid = !value.is_empty()
        && value.len() <= 20
        && !value.starts_with('0')
        && value.chars().all(|c| c.is_ascii_digit());
    if valid {
        value.to_string()
    } else {
        String::new()
    }
}

/// Whether the string is a bookmark key (`[0-9a-f]{64}`).
#[must_use]
pub fn is_bookmark_key(key: &str) -> bool {
    key.len() == 64
        && key
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Python `source_key`: the digest of `["source", source]`
/// where source is the provider id, else the playlist id.
#[must_use]
pub fn source_key(channel: &Channel) -> String {
    let source = match attr(channel, "provider-id") {
        "" => attr(channel, "playlist-id"),
        value => value,
    };
    if source.is_empty() {
        String::new()
    } else {
        digest(&py_json_list(&["source", source]))
    }
}

/// Python `legacy_key`: the digest of `[provider-id, url]`,
/// movies with a URL only.
#[must_use]
pub fn legacy_key(channel: &Channel) -> String {
    if !is_movie(channel) || channel.url.is_empty() {
        return String::new();
    }
    digest(&py_json_list(&[attr(channel, "provider-id"), &channel.url]))
}

/// Python `locator`: the validated Xtream locator, if the
/// channel carries a complete one.
#[must_use]
pub fn locator(channel: &Channel) -> Option<BookmarkLocator> {
    if attr(channel, "provider-type") != "xtream" {
        return None;
    }
    let source = source_key(channel);
    let kind = attr(channel, "resume-type");
    let id = identifier(attr(channel, "resume-id"));
    if source.is_empty() || id.is_empty() || !matches!(kind, "movie" | "episode") {
        return None;
    }
    let series = if kind == "episode" {
        let series = identifier(attr(channel, "series_id"));
        if series.is_empty() {
            return None;
        }
        Some(series)
    } else {
        None
    };
    Some(BookmarkLocator {
        source,
        kind: kind.to_string(),
        id,
        series,
    })
}

/// Python `item_key`: the record key for a channel — the
/// Xtream locator digest when a locator exists, else the
/// playlist-scoped digest, else the legacy digest; empty when
/// no key can be formed (no bookmark exists for the channel).
#[must_use]
pub fn item_key(channel: &Channel) -> String {
    let old = legacy_key(channel);
    if old.is_empty() {
        return String::new();
    }
    if let Some(locator) = locator(channel) {
        return digest(&format!(
            "[{}, {}]",
            py_json_string("xtream-v2"),
            py_json_locator(&locator)
        ));
    }
    if !attr(channel, "playlist-id").is_empty() && attr(channel, "provider-id").is_empty() {
        return digest(&py_json_list(&[
            "playlist-v2",
            &source_key(channel),
            &channel.url,
        ]));
    }
    old
}

/// Python `clean_metadata`'s title rule: stripped, non-empty,
/// no URL-ish or control characters, at most 240 chars.
fn clean_title(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let dirty = lowered.contains("://")
        || lowered.contains("www.")
        || raw.chars().any(|c| (c as u32) <= 0x1f);
    let title = raw.trim();
    if title.is_empty() || dirty {
        String::new()
    } else {
        title.chars().take(240).collect()
    }
}

/// Python `clean_metadata`'s locator rule, applied to a stored
/// (already parsed) locator object.
fn clean_locator(value: &Value) -> Option<BookmarkLocator> {
    let source = value.get("source").and_then(Value::as_str)?;
    if !is_bookmark_key(source) {
        return None;
    }
    let kind = value.get("type").and_then(Value::as_str)?;
    if !matches!(kind, "movie" | "episode") {
        return None;
    }
    let id = identifier(value.get("id").and_then(Value::as_str)?);
    if id.is_empty() {
        return None;
    }
    let series = if kind == "episode" {
        let series = identifier(value.get("series").and_then(Value::as_str)?);
        if series.is_empty() {
            return None;
        }
        Some(series)
    } else {
        None
    };
    Some(BookmarkLocator {
        source: source.to_string(),
        kind: kind.to_string(),
        id,
        series,
    })
}

/// The metadata a checkpoint stores for a channel (Python
/// `metadata(channel)`): the cleaned title and the locator.
#[must_use]
pub fn channel_metadata(channel: &Channel) -> (String, Option<BookmarkLocator>) {
    (clean_title(&channel.name), locator(channel))
}

fn json_int(value: Option<&Value>) -> Option<i64> {
    // Python requires `type(n) is int`: booleans and floats
    // never qualify, which `as_i64` reproduces.
    value.and_then(Value::as_i64)
}

/// Python `normalize`: only well-formed, unfinished records
/// survive, capped at the newest [`MAX_ENTRIES`].
#[must_use]
pub fn normalize(raw: Option<&Value>) -> Bookmarks {
    let mut result = Bookmarks::new();
    let Some(Value::Object(map)) = raw else {
        return result;
    };
    for (key, value) in map {
        if !is_bookmark_key(key) {
            continue;
        }
        let (Some(position_ms), Some(length_ms), Some(updated)) = (
            json_int(value.get("position_ms")),
            json_int(value.get("length_ms")),
            json_int(value.get("updated")),
        ) else {
            continue;
        };
        if !(MIN_POSITION_MS..length_ms).contains(&position_ms)
            || length_ms > MAX_MS
            || !(0..=i64::MAX).contains(&updated)
            || length_ms - position_ms <= MIN_POSITION_MS.max(length_ms / 20)
        {
            continue;
        }
        let metadata = value.get("metadata");
        let title = metadata
            .and_then(|m| m.get("title"))
            .and_then(Value::as_str)
            .map(clean_title)
            .unwrap_or_default();
        let locator = metadata
            .and_then(|m| m.get("locator"))
            .and_then(clean_locator);
        result.insert(
            key.clone(),
            Bookmark {
                position_ms,
                length_ms,
                updated,
                title,
                locator,
            },
        );
    }
    cap(&mut result);
    result
}

/// Keep the newest [`MAX_ENTRIES`] records, Python's
/// `(-updated, key)` order deciding eviction.
fn cap(entries: &mut Bookmarks) {
    if entries.len() > MAX_ENTRIES {
        let mut by_age: Vec<(String, i64)> = entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.updated))
            .collect();
        by_age.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (stale, _) in by_age.into_iter().skip(MAX_ENTRIES) {
            entries.remove(&stale);
        }
    }
}

/// Python `lookup`: the store with a legacy record migrated
/// to the channel's stable key when that is safe, the stored
/// metadata refreshed from the current channel, and the key
/// the channel's bookmark lives under (empty when the channel
/// can have no bookmark).
#[must_use]
pub fn lookup(entries: &Bookmarks, channel: &Channel) -> (Bookmarks, String) {
    let mut result = entries.clone();
    let key = item_key(channel);
    let old = legacy_key(channel);
    // A legacy record from an unscoped M3U cannot safely be
    // assigned to one of multiple playlists; preserve it
    // rather than guessing an account.
    let can_migrate = !attr(channel, "provider-id").is_empty() || key == old;
    if !key.is_empty() && old != key && can_migrate {
        if let Some(prior) = result.remove(&old) {
            let wins = result
                .get(&key)
                .is_none_or(|current| prior.updated > current.updated);
            if wins {
                result.insert(key.clone(), prior);
            }
        }
    }
    if let Some(entry) = result.get_mut(&key) {
        let (title, locator) = channel_metadata(channel);
        if !title.is_empty() || locator.is_some() {
            entry.title = title;
            entry.locator = locator;
        }
    }
    cap(&mut result);
    (result, key)
}

/// Python `checkpoint`: under 30 s, or inside the end window
/// (30 s or the last 5%), the record is removed as finished;
/// otherwise it is written with the channel's metadata. Bad
/// keys and out-of-range values change nothing.
#[must_use]
pub fn checkpoint(
    entries: &Bookmarks,
    key: &str,
    channel: &Channel,
    position_ms: i64,
    length_ms: i64,
    now: i64,
) -> Bookmarks {
    let mut result = entries.clone();
    if !is_bookmark_key(key)
        || !(0..=length_ms).contains(&position_ms)
        || length_ms > MAX_MS
        || length_ms == 0
        || !(0..=i64::MAX).contains(&now)
    {
        return result;
    }
    if position_ms < MIN_POSITION_MS
        || length_ms - position_ms <= MIN_POSITION_MS.max(length_ms / 20)
    {
        result.remove(key);
    } else {
        let (title, locator) = channel_metadata(channel);
        result.insert(
            key.to_string(),
            Bookmark {
                position_ms,
                length_ms,
                updated: now,
                title,
                locator,
            },
        );
    }
    cap(&mut result);
    result
}

/// Python `find_current`: the one unambiguous current item
/// for a key. Identical duplicate rows collapse; differing
/// source/URL duplicates make the key unavailable. No saved
/// stream URL is ever used — items come from the current
/// catalogue.
#[must_use]
pub fn find_current(key: &str, items: &[Channel]) -> Option<Channel> {
    let mut matches: Vec<(String, String, &Channel)> = Vec::new();
    for item in items {
        let new = item_key(item);
        let legacy = legacy_key(item);
        let hit = new == key
            || (legacy == key && (new == legacy || !attr(item, "provider-id").is_empty()));
        if hit {
            let identity = (new, item.url.clone());
            if !matches
                .iter()
                .any(|(n, u, _)| *n == identity.0 && *u == identity.1)
            {
                matches.push((identity.0, identity.1, item));
            }
        }
    }
    if matches.len() == 1 {
        matches.pop().map(|(_, _, channel)| channel.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(attrs: &[(&str, &str)], url: &str, name: &str) -> Channel {
        Channel {
            name: name.to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn xtream_movie() -> Channel {
        channel(
            &[
                ("provider-type", "xtream"),
                ("provider-id", "acct1"),
                ("resume-type", "movie"),
                ("resume-id", "12345"),
            ],
            "http://x/movie/u/p/12345.mp4",
            "Film",
        )
    }

    #[test]
    fn keys_match_the_python_digests() {
        // Golden values printed from playback_resume.py itself.
        let movie = xtream_movie();
        assert_eq!(
            item_key(&movie),
            "97c4f8d3c70511d8afc991a286e70f35eb110636b396bbce6dc62bf3b43fd988"
        );
        assert_eq!(
            legacy_key(&movie),
            "605f2ff100cad3d7e4a709c67c424ab3aff0304aff88c05b7a0d770c7e37cfc9"
        );
        assert_eq!(
            source_key(&movie),
            "87b86bbcb25c510896bc546f1ce8ce6901c6232c88567afc7e5b6de92fadb1c5"
        );
        let episode = channel(
            &[
                ("provider-type", "xtream"),
                ("provider-id", "acct1"),
                ("resume-type", "episode"),
                ("resume-id", "777"),
                ("series_id", "55"),
            ],
            "http://x/series/u/p/777.mp4",
            "Ep",
        );
        assert_eq!(
            item_key(&episode),
            "7306fd40ea0afd15dc250c9311ebc5c038a5c3b17336c90905d9d69b3fc2eda2"
        );
        let playlist = channel(&[("playlist-id", "pl1")], "http://y/film.mp4", "Pl Film");
        assert_eq!(
            item_key(&playlist),
            "8d34ebcf935add5aac41c1187d4a7c541eae27dd81e519ba4147f0ddc391f08e"
        );
        let legacy = channel(&[("provider-id", "acct9")], "http://z/m.mp4", "Legacy");
        assert_eq!(
            item_key(&legacy),
            "6b475d217818db934006d98a908359efe50394de3f27d2dd4053ecc5cdd18bca"
        );
        // Non-ASCII passes through unescaped (ensure_ascii=False).
        let uni = channel(&[("playlist-id", "plü")], "http://y/ü.mp4", "Uni é");
        assert_eq!(
            item_key(&uni),
            "fb633e122180ed806b4bbe15386f53b13ec998c1dd67623a0edc368ed0a23181"
        );
    }

    #[test]
    fn live_channels_have_no_bookmark_key() {
        let live = channel(&[("provider-id", "acct9")], "http://z/live/123", "Live");
        assert_eq!(item_key(&live), "");
    }

    #[test]
    fn normalize_drops_finished_short_and_malformed_records() {
        let raw = serde_json::json!({
            "a".repeat(64): {"position_ms": 900_000, "length_ms": 3_600_000, "updated": 5},
            "b".repeat(64): {"position_ms": 10_000, "length_ms": 3_600_000, "updated": 5},
            "c".repeat(64): {"position_ms": 3_590_000, "length_ms": 3_600_000, "updated": 5},
            "d".repeat(64): {"position_ms": 900_000.0, "length_ms": 3_600_000, "updated": 5},
            "not-a-key": {"position_ms": 900_000, "length_ms": 3_600_000, "updated": 5},
        });
        let entries = normalize(Some(&raw));
        assert_eq!(entries.len(), 1);
        assert!(entries.contains_key(&"a".repeat(64)));
    }

    #[test]
    fn checkpoint_writes_and_removes_by_the_python_bounds() {
        let movie = xtream_movie();
        let key = item_key(&movie);
        let entries = checkpoint(&Bookmarks::new(), &key, &movie, 900_000, 3_600_000, 42);
        let entry = entries.get(&key).expect("written");
        assert_eq!(entry.position_ms, 900_000);
        assert_eq!(entry.title, "Film");
        assert_eq!(entry.locator.as_ref().map(|l| l.id.as_str()), Some("12345"));
        // Inside the end window: finished, record removed.
        let entries = checkpoint(&entries, &key, &movie, 3_590_000, 3_600_000, 43);
        assert!(!entries.contains_key(&key));
        // A non-hex key (the old channel_key shape) writes nothing.
        let entries = checkpoint(&entries, "acct1|Film", &movie, 900_000, 3_600_000, 44);
        assert!(entries.is_empty());
    }

    #[test]
    fn lookup_migrates_a_legacy_record_to_the_stable_key() {
        let movie = xtream_movie();
        let mut raw_entries = Bookmarks::new();
        raw_entries.insert(
            legacy_key(&movie),
            Bookmark {
                position_ms: 900_000,
                length_ms: 3_600_000,
                updated: 7,
                title: String::new(),
                locator: None,
            },
        );
        let (migrated, key) = lookup(&raw_entries, &movie);
        assert_eq!(key, item_key(&movie));
        assert!(!migrated.contains_key(&legacy_key(&movie)));
        let entry = migrated.get(&key).expect("migrated");
        assert_eq!(entry.position_ms, 900_000);
        // Metadata refreshed from the current channel.
        assert_eq!(entry.title, "Film");
    }

    #[test]
    fn find_current_requires_one_unambiguous_item() {
        let movie = xtream_movie();
        let key = item_key(&movie);
        assert_eq!(
            find_current(&key, std::slice::from_ref(&movie)),
            Some(movie.clone())
        );
        // Identical duplicates collapse.
        assert_eq!(
            find_current(&key, &[movie.clone(), movie.clone()]),
            Some(movie.clone())
        );
        // A differing URL under the same identity is ambiguous.
        let mut other = movie.clone();
        other.url = "http://x/movie/u/p/99999.mp4".to_string();
        assert_eq!(find_current(&key, &[movie.clone(), other]), None);
        assert_eq!(find_current(&key, &[]), None);
    }
}
