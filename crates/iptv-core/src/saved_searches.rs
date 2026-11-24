//! Saved programme searches, ported from Python `saved_searches.py`.
//! Up to 30 rules (2-100 characters each) over the guide's next 14
//! days, scoped to all channels or a chosen set of channel keys;
//! matches alert once (the `programme-search-state.json` history
//! full backups carry) and can create reminders or recordings.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::epg::{match_guide_channel, GuideIndexes, Programme};
use crate::epgstore::EpgStore;
use crate::Channel;

/// Python `MAX_RULES`.
pub const MAX_RULES: usize = 30;
/// Python `MAX_RESULTS`.
pub const MAX_RESULTS: usize = 500;
/// Python `MAX_SEEN`.
pub const MAX_SEEN: usize = 20_000;
/// Python scan window: now to now + 14 days.
pub const SCAN_DAYS: i64 = 14;
/// Python "choose at most 2000 channels".
pub const MAX_CHANNELS_PER_RULE: usize = 2000;

const DAMAGED: &str = "The saved search history is damaged.";
const BAD_RULE: &str = "Enter a search of 2 to 100 characters and choose at most 2000 channels.";

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_hex32(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One saved-search rule, as stored under the config key
/// `saved_programme_searches` (Python's row shape).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    /// 32 lowercase hex characters (Python `uuid4().hex`).
    pub id: String,
    /// The search text, trimmed (2-100 characters).
    pub query: String,
    /// Channel keys scoping the rule; empty means all channels.
    pub channels: Vec<String>,
    /// Whether descriptions are searched too.
    pub description: bool,
    /// Whether the rule runs.
    pub enabled: bool,
}

impl Rule {
    /// The config-row JSON for this rule.
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "query": self.query,
            "channels": self.channels,
            "description": self.description,
            "enabled": self.enabled,
        })
    }
}

/// Python `normalize`: keep only well-formed rows, first
/// [`MAX_RULES`] of them, dropping duplicate ids and malformed
/// scopes exactly as the reference does.
#[must_use]
pub fn normalize(value: &Value) -> Vec<Rule> {
    let Some(rows) = value.as_array() else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for raw in rows.iter().take(MAX_RULES) {
        let Some(row) = raw.as_object() else {
            continue;
        };
        let Some(key) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(query) = row.get("query").and_then(Value::as_str) else {
            continue;
        };
        let trimmed = query.trim();
        if !is_hex32(key)
            || seen.contains(key)
            || !(2..=100).contains(&trimmed.chars().count())
            || query.chars().any(|c| c < '\u{20}')
        {
            continue;
        }
        let Some(scope) = row.get("channels").and_then(Value::as_array) else {
            continue;
        };
        if scope.len() > MAX_CHANNELS_PER_RULE
            || scope
                .iter()
                .any(|k| k.as_str().is_none_or(|k| !is_hex64(k)))
        {
            continue;
        }
        seen.insert(key.to_string());
        let mut channels: Vec<String> = Vec::new();
        for key in scope {
            let key = key.as_str().unwrap_or_default().to_string();
            if !channels.contains(&key) {
                channels.push(key);
            }
        }
        out.push(Rule {
            id: key.to_string(),
            query: trimmed.to_string(),
            channels,
            description: row.get("description") == Some(&Value::Bool(true)),
            enabled: row.get("enabled").is_none_or(|e| e == &Value::Bool(true)),
        });
    }
    out
}

/// A fresh rule id: 16 random bytes as 32 hex chars, Python's
/// `uuid4().hex` shape (the `uuid` crate here has no `v4` feature).
fn fresh_id() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Fall back to a counter hashed; ids only need to be unique
        // within this device's rule set.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        bytes.copy_from_slice(&Sha256::digest(n.to_string().as_bytes())[..16]);
    }
    let mut out = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Python `new_rule`: a fresh id, validated by [`normalize`].
///
/// # Errors
/// Returns Python's message when the query or scope is invalid.
pub fn new_rule(query: &str, channels: &[String], description: bool) -> Result<Rule, String> {
    let raw = serde_json::json!([{
        "id": fresh_id(),
        "query": query,
        "channels": channels,
        "description": description,
        "enabled": true,
    }]);
    normalize(&raw)
        .into_iter()
        .next()
        .ok_or_else(|| BAD_RULE.to_string())
}

/// Python `signature`: sha256 over the normalized rules JSON.
#[must_use]
pub fn signature(rules: &[Rule]) -> String {
    let value = Value::Array(rules.iter().map(Rule::to_json).collect());
    sha256_hex(serde_json::to_string(&value).unwrap_or_default().as_bytes())
}

/// One programme match, as the results dialog shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match {
    /// sha256 of `[rule_id, guide channel id, start stamp]`.
    pub id: String,
    /// The rule this match belongs to.
    pub rule: String,
    /// Every playlist channel key carrying this guide slot.
    pub channel_keys: Vec<String>,
    /// Display name of the first such channel.
    pub channel: String,
    /// The guide channel id of the programme.
    pub channel_id: String,
    /// The programme title (safe-title form).
    pub title: String,
    /// Programme start, epoch seconds.
    pub start: i64,
    /// Programme end, epoch seconds.
    pub end: i64,
    /// The programme description (up to 16,000 characters).
    pub description: String,
    /// Episode detail, for recording scheduling.
    pub episode: String,
    /// Subtitle detail, for recording scheduling.
    pub subtitle: String,
}

impl Match {
    /// The guide [`Programme`] this match carries, for the
    /// reminder/recording builders.
    #[must_use]
    pub fn programme(&self) -> Programme {
        Programme {
            channel_id: self.channel_id.clone(),
            start: self.start,
            stop: self.end,
            title: self.title.clone(),
            description: self.description.clone(),
            episode: self.episode.clone(),
            subtitle: self.subtitle.clone(),
        }
    }
}

/// One rule's scan result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleResult {
    /// The rule id.
    pub rule: String,
    /// The rule query.
    pub query: String,
    /// At most [`MAX_RESULTS`] matches, in start order.
    pub matches: Vec<Match>,
    /// Whether more matches exist beyond [`MAX_RESULTS`].
    pub truncated: bool,
}

/// Python `scan`: one read-only pass over the guide store.
/// `channels` is the playlist catalogue; `now` is epoch seconds.
/// Video-on-demand entries are not in this catalogue at all, so
/// Python's kind guard is structural here.
///
/// # Errors
/// Returns the store error text when the guide cannot be read.
pub fn scan(
    store: &EpgStore,
    rules: &[Rule],
    channels: &[Channel],
    now: i64,
) -> Result<Vec<RuleResult>, String> {
    let guides = store.channels()?;
    let indexes = GuideIndexes::build(&guides);
    let enabled: Vec<&Rule> = rules.iter().filter(|r| r.enabled).collect();
    // Guide id -> playlist channel keys (and display names).
    let mut mapped: HashMap<String, HashMap<String, String>> = HashMap::new();
    for channel in channels {
        let Some(guide_id) =
            match_guide_channel::<std::collections::hash_map::RandomState>(channel, &indexes, None)
        else {
            continue;
        };
        let key = crate::reminders::reminder_channel_key(channel);
        if key.is_empty() {
            continue;
        }
        let entry = mapped.entry(guide_id).or_default();
        entry.insert(key, crate::reminders::safe_title(&channel.name));
    }
    let from = now;
    let to = now + SCAN_DAYS * 86_400;
    let mut output = Vec::new();
    for rule in &enabled {
        let scope: HashSet<&str> = rule.channels.iter().map(String::as_str).collect();
        let mut matches = Vec::new();
        let mut identities: HashSet<String> = HashSet::new();
        let mut truncated = false;
        let has_scope_match = scope.is_empty()
            || mapped
                .values()
                .any(|entry| entry.keys().any(|key| scope.contains(key.as_str())));
        if has_scope_match {
            let rows = store.saved_search_programmes(&rule.query, rule.description, from, to)?;
            for programme in rows {
                if programme.stop <= programme.start {
                    continue;
                }
                let mut eligible: Vec<(&String, &String)> = mapped
                    .get(&programme.channel_id)
                    .map_or_else(Vec::new, |entry| {
                        entry
                            .iter()
                            .filter(|(key, _)| scope.is_empty() || scope.contains(key.as_str()))
                            .collect()
                    });
                if eligible.is_empty() {
                    continue;
                }
                // One notification per guide slot, even when several
                // playlist variants carry it (Python's identity).
                let stamp = chrono::TimeZone::timestamp_opt(&chrono::Utc, programme.start, 0)
                    .single()
                    .map_or_else(
                        || programme.start.to_string(),
                        |dt| dt.format("%Y%m%d%H%M%S").to_string(),
                    );
                let identity = sha256_hex(
                    serde_json::json!([rule.id, programme.channel_id, stamp])
                        .to_string()
                        .as_bytes(),
                );
                if !identities.insert(identity.clone()) {
                    continue;
                }
                if matches.len() == MAX_RESULTS {
                    truncated = true;
                    break;
                }
                eligible.sort_by(|a, b| a.0.cmp(b.0));
                let channel_keys: Vec<String> =
                    eligible.iter().map(|(k, _)| (*k).clone()).collect();
                let channel = eligible[0].1.clone();
                matches.push(Match {
                    id: identity,
                    rule: rule.id.clone(),
                    channel_keys,
                    channel,
                    channel_id: programme.channel_id.clone(),
                    title: crate::reminders::safe_title(&programme.title),
                    start: programme.start,
                    end: programme.stop,
                    description: programme.description.chars().take(16_000).collect(),
                    episode: programme.episode,
                    subtitle: programme.subtitle,
                });
            }
        }
        output.push(RuleResult {
            rule: rule.id.clone(),
            query: rule.query.clone(),
            matches,
            truncated,
        });
    }
    Ok(output)
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
pub fn read_seen(path: &Path, now: f64) -> Result<Value, String> {
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

/// Python `commit_seen`: merge the current matches into the history
/// and write it back (temp file + rename, fsynced); the count of
/// new matches is returned only after the write lands, so a failure
/// cannot erase state or over-report.
///
/// # Errors
/// Returns Python's history message for a damaged old value, or the
/// OS error text when the write fails.
pub fn commit_seen(
    path: &Path,
    old: &Value,
    results: &[RuleResult],
    now: f64,
) -> Result<(Value, usize), String> {
    use std::io::Write as _;
    let mut seen = normalize_seen(old, now)?
        .as_object()
        .cloned()
        .unwrap_or_default();
    let matches: Vec<(&String, i64)> = results
        .iter()
        .flat_map(|r| r.matches.iter().map(|m| (&m.id, m.end)))
        .collect();
    let match_ids: HashSet<&String> = matches.iter().map(|(id, _)| *id).collect();
    let count = matches
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !seen.contains_key(*id))
        .count();
    for (id, end) in &matches {
        seen.insert((*id).clone(), serde_json::json!(*end));
    }
    // Keep every current match before older history, so a full
    // history cannot evict a still-visible future match and
    // re-alert about it on every scan.
    let mut rows: Vec<(String, Value)> = seen.into_iter().collect();
    rows.sort_by(|a, b| {
        (!match_ids.contains(&a.0))
            .cmp(&(!match_ids.contains(&b.0)))
            .then_with(|| {
                let at = |v: &Value| v.as_f64().unwrap_or(f64::INFINITY);
                at(&a.1).total_cmp(&at(&b.1))
            })
    });
    rows.truncate(MAX_SEEN);
    let seen = normalize_seen(&Value::Object(rows.into_iter().collect()), now)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(directory) = parent {
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("search");
    let temporary = path.with_file_name(format!("{stem}-{}.tmp", fresh_id()));
    let write = (|| -> Result<(), String> {
        let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(serde_json::to_string(&seen).unwrap_or_default().as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write?;
    Ok((seen, count))
}

/// Python's channel-keyed resolve used by the dialog's
/// Set Reminder / Schedule Recording actions: find the playlist
/// channel for each key, in order.
#[must_use]
pub fn resolve_channels<'a>(keys: &[String], channels: &'a [Channel]) -> Vec<&'a Channel> {
    let wanted: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let mut found: Vec<&Channel> = Vec::new();
    for channel in channels {
        let key = crate::reminders::reminder_channel_key(channel);
        if wanted.contains(key.as_str()) {
            found.push(channel);
        }
    }
    // Keep key order, then any leftovers in catalogue order.
    found.sort_by_key(|channel| {
        let key = crate::reminders::reminder_channel_key(channel);
        keys.iter().position(|k| k == &key).unwrap_or(usize::MAX)
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(n: u8) -> String {
        format!("{n:02x}").repeat(16)
    }

    fn hex64(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    #[test]
    fn normalize_keeps_valid_and_drops_bad() {
        let value = serde_json::json!([
            {"id": hex32(0x1a), "query": "  Nova  ", "channels": [hex64(0x2b), hex64(0x2b)],
             "description": true, "enabled": false},
            {"id": "short", "query": "Nova", "channels": []},
            {"id": hex32(0x1a), "query": "Again", "channels": []},
            {"id": hex32(0x3c), "query": "x", "channels": []},
            {"id": hex32(0x4d), "query": "Long enough", "channels": ["nope"]},
            {"id": hex32(0x5e), "query": "Bad\nquery", "channels": []},
            {"id": hex32(0x6f), "query": "Second valid", "channels": []},
        ]);
        let rules = normalize(&value);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].query, "Nova");
        assert_eq!(rules[0].channels, vec![hex64(0x2b)]);
        assert!(rules[0].description);
        assert!(!rules[0].enabled);
        assert!(rules[1].enabled);
        assert!(normalize(&serde_json::json!([])).is_empty());
        assert!(normalize(&serde_json::json!(null)).is_empty());
    }

    #[test]
    fn normalize_caps_rules_and_scope() {
        let many: Vec<Value> = (0..35_u8)
            .map(|n| serde_json::json!({"id": hex32(n), "query": "Nova show", "channels": []}))
            .collect();
        assert_eq!(normalize(&Value::Array(many)).len(), MAX_RULES);
        let wide = serde_json::json!([{
            "id": hex32(0xff), "query": "Nova show",
            "channels": vec![hex64(0); MAX_CHANNELS_PER_RULE + 1],
        }]);
        assert!(normalize(&wide).is_empty());
    }

    #[test]
    fn new_rule_round_trips_and_rejects() {
        let rule = new_rule("  My show  ", &[hex64(1)], false).unwrap();
        assert_eq!(rule.query, "My show");
        assert!(is_hex32(&rule.id));
        assert!(new_rule("x", &[], false).is_err());
        assert!(new_rule("ok query", &["bad".to_string()], false).is_err());
    }

    #[test]
    fn signature_is_stable_and_sensitive() {
        let rule = new_rule("Nova show", &[], true).unwrap();
        assert_eq!(
            signature(std::slice::from_ref(&rule)),
            signature(std::slice::from_ref(&rule))
        );
        let mut other = rule.clone();
        other.query = "Other show".to_string();
        assert_ne!(
            signature(std::slice::from_ref(&rule)),
            signature(std::slice::from_ref(&other))
        );
    }

    #[test]
    fn seen_keeps_only_upcoming_ids() {
        let now = 1_000_000.0;
        let value = serde_json::json!({
            hex64(0xa): now + 60.0,
            hex64(0xb): now - 1.0,
            hex64(0xc): now + 17.0 * 86_400.0,
            "short": now + 60.0,
            hex64(0xd): "later",
        });
        let kept = normalize_seen(&value, now).unwrap();
        assert_eq!(kept, serde_json::json!({ hex64(0xa): now + 60.0 }));
        assert!(normalize_seen(&serde_json::json!([]), now).is_err());
    }

    #[test]
    fn commit_seen_counts_new_and_persists() {
        let dir = std::env::temp_dir().join(format!("ss-test-{}", std::process::id()));
        let path = dir.join("programme-search-state.json");
        let _ = std::fs::remove_dir_all(&dir);
        let now = 1_700_000_000.0;
        let result = RuleResult {
            rule: hex32(1),
            query: "Nova".to_string(),
            matches: vec![Match {
                id: hex64(1),
                rule: hex32(1),
                channel_keys: vec![hex64(2)],
                channel: "One".to_string(),
                channel_id: "one".to_string(),
                title: "Nova".to_string(),
                start: 1_700_000_100,
                end: 1_700_000_200,
                description: String::new(),
                episode: String::new(),
                subtitle: String::new(),
            }],
            truncated: false,
        };
        let (seen, count) = commit_seen(&path, &serde_json::json!({}), &[result], now).unwrap();
        assert_eq!(count, 1);
        assert_eq!(seen[hex64(1)], serde_json::json!(1_700_000_200));
        assert_eq!(read_seen(&path, now).unwrap(), seen);
        // Second commit: the same match is no longer new.
        let result2 = RuleResult {
            rule: hex32(1),
            query: "Nova".to_string(),
            matches: vec![Match {
                id: hex64(1),
                rule: hex32(1),
                channel_keys: vec![hex64(2)],
                channel: "One".to_string(),
                channel_id: "one".to_string(),
                title: "Nova".to_string(),
                start: 1_700_000_100,
                end: 1_700_000_200,
                description: String::new(),
                episode: String::new(),
                subtitle: String::new(),
            }],
            truncated: false,
        };
        let (_, count2) = commit_seen(&path, &seen, &[result2], now).unwrap();
        assert_eq!(count2, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_store() -> EpgStore {
        let mut guide = crate::epg::EpgGuide::default();
        guide.channels.push(crate::epg::EpgChannel {
            id: "one".to_string(),
            display_name: "Channel One".to_string(),
        });
        let base = 1_700_000_000_i64;
        let mut rows = Vec::new();
        for (offset, title) in [
            (100, "Nova Nights"),
            (200, "Other Show"),
            (300, "nova early"),
        ] {
            rows.push(crate::epg::Programme {
                channel_id: "one".to_string(),
                start: base + offset,
                stop: base + offset + 50,
                title: title.to_string(),
                description: format!("About {title}"),
                episode: String::new(),
                subtitle: String::new(),
            });
        }
        guide.programmes.insert("one".to_string(), rows);
        let mut store = EpgStore::open(
            &std::env::temp_dir().join(format!("ss-guide-{}.db", std::process::id())),
        )
        .unwrap();
        store.import_guide(&guide).unwrap();
        store
    }

    fn sample_channel() -> Channel {
        Channel {
            name: "Channel One".to_string(),
            group: String::new(),
            url: "http://example/one".to_string(),
            tvg_id: "one".to_string(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::default(),
        }
    }

    #[test]
    fn scan_finds_titles_and_scopes() {
        let store = sample_store();
        let now = 1_700_000_000_i64;
        let channel = sample_channel();
        let key = crate::reminders::reminder_channel_key(&channel);
        // All-channels rule: case-insensitive title matches, windowed.
        let rule = new_rule("NOVA", &[], false).unwrap();
        let results = scan(&store, &[rule], std::slice::from_ref(&channel), now).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 2);
        assert!(!results[0].truncated);
        assert_eq!(results[0].matches[0].channel, "Channel One");
        assert_eq!(results[0].matches[0].channel_keys, vec![key.clone()]);
        // Scoped to a different key: nothing.
        let scoped = new_rule("Nova", &[hex64(9)], false).unwrap();
        let results = scan(&store, &[scoped], std::slice::from_ref(&channel), now).unwrap();
        assert!(results[0].matches.is_empty());
        // Scoped to the channel's key: matches again.
        let scoped = new_rule("Nova", std::slice::from_ref(&key), false).unwrap();
        let results = scan(&store, &[scoped], std::slice::from_ref(&channel), now).unwrap();
        assert_eq!(results[0].matches.len(), 2);
        // Description search reaches description text.
        let desc = new_rule("About Other", &[], true).unwrap();
        let results = scan(&store, &[desc], &[sample_channel()], now).unwrap();
        assert_eq!(results[0].matches.len(), 1);
        // Disabled rules do not run.
        let mut off = new_rule("Nova", &[], false).unwrap();
        off.enabled = false;
        let results = scan(&store, &[off], &[sample_channel()], now).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn resolve_channels_orders_by_key() {
        let channel = sample_channel();
        let key = crate::reminders::reminder_channel_key(&channel);
        let channels = [channel];
        let found = resolve_channels(&[key], &channels);
        assert_eq!(found.len(), 1);
        assert!(resolve_channels(&[hex64(0)], &[sample_channel()]).is_empty());
    }
}
